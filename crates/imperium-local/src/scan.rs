//! Finds GGUF models in the configured folders.
//!
//! Layouts recognised besides plain folders of files: LM Studio's and the download folder's
//! `<publisher>/<repo>/<file>.gguf`, and the Hugging Face cache
//! (`hub/models--<org>--<name>/snapshots/<revision>/...`), where only the revision `refs/main`
//! names is scanned. Hidden folders (the cache's `.no_exist` placeholders, `.locks`), the
//! cache's `blobs`, diffusers pipelines (`model_index.json`), and GGUF files that are not
//! language models (image models such as FLUX) are left out.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::gguf::{GgufInfo, read_info};

/// A model found in a model folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModel {
    /// The file name without `.gguf` (and without the split suffix); used in
    /// `llamacpp:<id>` specs.
    pub id: String,
    /// The model file, or the first part of a split model.
    pub path: PathBuf,
    pub parts: u32,
    /// Total size of all parts: the weights' memory footprint.
    pub size_bytes: u64,
    pub info: GgufInfo,
    /// A multimodal projector in the same folder, passed to llama.cpp as `--mmproj`.
    pub mmproj: Option<PathBuf>,
    pub format: Format,
    /// The repository the model came from, such as `unsloth/Qwen3-8B-GGUF`: from the Hugging
    /// Face cache's folder name, or the folders above the file (`<publisher>/<repo>`).
    pub repo: Option<String>,
}

/// How a model is stored, which decides the runtime that serves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A GGUF file, served by llama.cpp.
    Gguf,
    /// A Hugging Face folder (`config.json` and safetensors), served by vLLM.
    Safetensors,
}

/// Something skipped while scanning, reported rather than failing the scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub models: Vec<LocalModel>,
    pub skipped: Vec<Skipped>,
}

/// A GGUF file or a Hugging Face model folder found under a model folder, with the repository
/// it belongs to.
#[derive(Debug)]
struct Found {
    path: PathBuf,
    repo: Option<String>,
}

#[derive(Debug, Default)]
struct Walk {
    files: Vec<Found>,
    hf_dirs: Vec<Found>,
}

/// Scans `folders` recursively. Symbolic links to folders are not followed, so loops are
/// impossible; when two files share an id, the first folder listed wins.
pub fn scan(folders: &[PathBuf]) -> Scan {
    let mut scan = Scan::default();
    let mut ids = BTreeMap::new();
    for folder in folders {
        let mut walk = Walk::default();
        collect(folder, folder, None, &mut walk, &mut scan.skipped);
        walk.files.sort_by(|a, b| a.path.cmp(&b.path));
        walk.hf_dirs.sort_by(|a, b| a.path.cmp(&b.path));
        for dir_files in group_by_dir(&walk.files) {
            add_dir(&dir_files, &mut scan, &mut ids);
        }
        for dir in &walk.hf_dirs {
            add_hf_dir(dir, &mut scan, &mut ids);
        }
    }
    scan.models.sort_by(|a, b| a.id.cmp(&b.id));
    scan
}

/// The repository a file in `dir` belongs to, from its place under `root`: the two folders
/// above it (`<publisher>/<repo>`), or the one when it is a single level down.
fn repo_from_layout(root: &Path, dir: &Path) -> Option<String> {
    let relative = dir.strip_prefix(root).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    match parts.as_slice() {
        [] => None,
        [only] => Some(only.clone()),
        [.., publisher, repo] => Some(format!("{publisher}/{repo}")),
    }
}

/// `models--<org>--<name>` (a Hugging Face cache folder) as `<org>/<name>`.
pub(crate) fn hf_cache_repo(name: &str) -> Option<String> {
    let rest = name.strip_prefix("models--")?;
    let (org, repo) = rest.split_once("--")?;
    (!org.is_empty() && !repo.is_empty()).then(|| format!("{org}/{repo}"))
}

/// The snapshots of a Hugging Face cache repository to scan: the one `refs/main` names when it
/// is there, else every snapshot.
fn cache_snapshots(repo_dir: &Path) -> Vec<PathBuf> {
    let snapshots = repo_dir.join("snapshots");
    if let Ok(main) = fs::read_to_string(repo_dir.join("refs").join("main")) {
        let main = snapshots.join(main.trim());
        if main.is_dir() {
            return vec![main];
        }
    }
    let mut all: Vec<PathBuf> = fs::read_dir(&snapshots)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    all.sort();
    all
}

/// Gathers GGUF files, and Hugging Face model folders (not searched further), under `dir`.
/// `cache_repo` is set inside a Hugging Face cache snapshot.
fn collect(
    root: &Path,
    dir: &Path,
    cache_repo: Option<&str>,
    walk: &mut Walk,
    skipped: &mut Vec<Skipped>,
) {
    let repo = || {
        cache_repo
            .map(str::to_string)
            .or_else(|| repo_from_layout(root, dir))
    };
    if dir.join("model_index.json").is_file() {
        skipped.push(Skipped {
            path: dir.to_path_buf(),
            reason: "an image generation pipeline (diffusers), not a language model".into(),
        });
        return;
    }
    if is_hf_dir(dir) {
        walk.hf_dirs.push(Found {
            path: dir.to_path_buf(),
            repo: repo(),
        });
        return;
    }
    if cache_repo.is_none()
        && let Some(cache) = dir
            .file_name()
            .and_then(|name| hf_cache_repo(&name.to_string_lossy()))
    {
        for snapshot in cache_snapshots(dir) {
            collect(root, &snapshot, Some(&cache), walk, skipped);
        }
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            skipped.push(Skipped {
                path: dir.to_path_buf(),
                reason: error.to_string(),
            });
            return;
        }
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            // Hidden folders hold caches and placeholders (`.no_exist`, `.locks`, `.cache`).
            if !entry.file_name().to_string_lossy().starts_with('.') {
                collect(root, &path, cache_repo, walk, skipped);
            }
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
        {
            walk.files.push(Found { path, repo: repo() });
        }
    }
}

fn group_by_dir(files: &[Found]) -> Vec<Vec<&Found>> {
    let mut dirs: BTreeMap<&Path, Vec<&Found>> = BTreeMap::new();
    for file in files {
        dirs.entry(file.path.parent().unwrap_or(Path::new("")))
            .or_default()
            .push(file);
    }
    dirs.into_values().collect()
}

fn is_projector(stem: &str) -> bool {
    stem.to_ascii_lowercase().contains("mmproj")
}

/// Letters and digits only, lower case: names compared without separators.
fn squash(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The projector for the model named `id` among `projectors`: the only one, else the one whose
/// name (without `mmproj`) shares the longest start with the model's.
fn pick_projector(id: &str, projectors: &[(String, PathBuf)]) -> Option<PathBuf> {
    if projectors.len() <= 1 {
        return projectors.first().map(|(_, path)| path.clone());
    }
    let model = squash(id);
    projectors
        .iter()
        .max_by_key(|(stem, _)| {
            let name = squash(&stem.to_ascii_lowercase().replace("mmproj", ""));
            model
                .chars()
                .zip(name.chars())
                .take_while(|(a, b)| a == b)
                .count()
        })
        .map(|(_, path)| path.clone())
}

/// Why a GGUF header is not a model to chat with, if it is not: image models (FLUX, Qwen-Image,
/// Stable Diffusion) are GGUF files too, named by their architecture, or with no transformer
/// blocks or context length.
pub fn not_a_language_model(info: &GgufInfo) -> Option<String> {
    const IMAGE: &[&str] = &[
        "flux",
        "sd1",
        "sd3",
        "sdxl",
        "qwen_image",
        "wan",
        "ltxv",
        "hidream",
        "chroma",
        "lumina2",
        "hyvid",
        "cosmos",
        "aura",
    ];
    // A header that names no architecture says too little to judge, so it is kept.
    let arch = info.architecture.as_deref()?;
    let image = IMAGE.contains(&arch);
    (image || (info.block_count.is_none() && info.context_length.is_none())).then(|| {
        format!(
            "not a language model (architecture {arch}{}), so it cannot be used for chat",
            if image { ", an image model" } else { "" }
        )
    })
}

fn add_dir(files: &[&Found], scan: &mut Scan, ids: &mut BTreeMap<String, PathBuf>) {
    let stem = |path: &Path| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let projectors: Vec<(String, PathBuf)> = files
        .iter()
        .map(|found| (stem(&found.path), found.path.clone()))
        .filter(|(name, _)| is_projector(name))
        .collect();
    for found in files {
        let path = &found.path;
        let name = stem(path);
        if is_projector(&name) {
            continue;
        }
        let (id, parts) = match split_part(&name) {
            Some((base, 1, count)) => (base.to_string(), count),
            Some(_) => continue, // later parts are counted with the first
            None => (name.clone(), 1),
        };
        let skip = |reason: String| Skipped {
            path: path.to_path_buf(),
            reason,
        };
        let part_paths: Vec<PathBuf> = if parts == 1 {
            vec![path.to_path_buf()]
        } else {
            (1..=parts)
                .map(|n| path.with_file_name(format!("{id}-{n:05}-of-{parts:05}.gguf")))
                .collect()
        };
        let mut size_bytes = 0;
        let mut missing = None;
        for part in &part_paths {
            match fs::metadata(part) {
                Ok(meta) => size_bytes += meta.len(),
                Err(_) => missing = Some(part.clone()),
            }
        }
        if let Some(part) = missing {
            scan.skipped
                .push(skip(format!("split model is missing {}", part.display())));
            continue;
        }
        let info = match read_info(path) {
            Ok(info) => info,
            Err(error) => {
                scan.skipped
                    .push(skip(format!("not a readable GGUF file: {error}")));
                continue;
            }
        };
        if let Some(reason) = not_a_language_model(&info) {
            scan.skipped.push(skip(reason));
            continue;
        }
        if let Some(first) = ids.get(&id) {
            scan.skipped.push(skip(format!(
                "id {id:?} is already used by {}",
                first.display()
            )));
            continue;
        }
        ids.insert(id.clone(), path.to_path_buf());
        scan.models.push(LocalModel {
            mmproj: pick_projector(&id, &projectors),
            id,
            path: path.to_path_buf(),
            parts,
            size_bytes,
            info,
            format: Format::Gguf,
            repo: found.repo.clone(),
        });
    }
}

fn is_hf_dir(dir: &Path) -> bool {
    dir.join("config.json").is_file()
        && fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "safetensors")
            })
        })
}

/// A revision folder of the Hugging Face cache: 40 hexadecimal digits.
fn is_revision(name: &str) -> bool {
    name.len() == 40 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Adds a Hugging Face model folder, reading its shape from `config.json`.
fn add_hf_dir(found: &Found, scan: &mut Scan, ids: &mut BTreeMap<String, PathBuf>) {
    let dir = found.path.as_path();
    let skip = |reason: String| Skipped {
        path: dir.to_path_buf(),
        reason,
    };
    let folder = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // In the Hugging Face cache the folder is a revision; the repository names the model.
    let id = match &found.repo {
        Some(repo) if is_revision(&folder) => repo.rsplit('/').next().unwrap_or(repo).to_string(),
        _ => folder,
    };
    let config: serde_json::Value = match fs::read_to_string(dir.join("config.json"))
        .map_err(|error| error.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
    {
        Ok(config) => config,
        Err(error) => {
            scan.skipped.push(skip(format!("config.json: {error}")));
            return;
        }
    };
    if let Some(first) = ids.get(&id) {
        scan.skipped.push(skip(format!(
            "id {id:?} is already used by {}",
            first.display()
        )));
        return;
    }
    let size_bytes = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "safetensors")
        })
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum();
    // Multimodal configs nest the language model's shape under text_config.
    let shape = config.get("text_config").unwrap_or(&config);
    let number = |key: &str| shape.get(key).and_then(serde_json::Value::as_u64);
    let info = GgufInfo {
        architecture: config
            .get("architectures")
            .and_then(|list| list.get(0))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        name: Some(id.clone()),
        block_count: number("num_hidden_layers"),
        embedding_length: number("hidden_size"),
        head_count: number("num_attention_heads"),
        head_count_kv: number("num_key_value_heads"),
        key_length: number("head_dim"),
        value_length: number("head_dim"),
        context_length: number("max_position_embeddings"),
        ..GgufInfo::default()
    };
    ids.insert(id.clone(), dir.to_path_buf());
    scan.models.push(LocalModel {
        id,
        path: dir.to_path_buf(),
        parts: 1,
        size_bytes,
        info,
        mmproj: None,
        format: Format::Safetensors,
        repo: found.repo.clone(),
    });
}

/// Splits `name-00001-of-00003` into (`name`, 1, 3).
pub(crate) fn split_part(stem: &str) -> Option<(&str, u32, u32)> {
    let (rest, count) = stem.rsplit_once("-of-")?;
    let (base, index) = rest.rsplit_once('-')?;
    let digits = |text: &str| text.len() == 5 && text.bytes().all(|b| b.is_ascii_digit());
    if !digits(index) || !digits(count) {
        return None;
    }
    Some((base, index.parse().ok()?, count.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::fixture::llama;

    #[test]
    fn finds_models_split_sets_and_projectors() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("vision/qwen");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.path().join("small-q4.gguf"), llama("Small")).unwrap();
        fs::write(nested.join("qwen-vl.gguf"), llama("VL")).unwrap();
        fs::write(nested.join("mmproj-qwen-vl-f16.gguf"), llama("proj")).unwrap();
        for n in 1..=2 {
            fs::write(
                root.path().join(format!("big-{n:05}-of-00002.gguf")),
                llama("Big"),
            )
            .unwrap();
        }
        fs::write(root.path().join("notes.txt"), "ignored").unwrap();
        fs::write(root.path().join("broken.gguf"), "not a model").unwrap();

        let scan = scan(&[root.path().to_path_buf()]);
        let ids: Vec<_> = scan.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["big", "qwen-vl", "small-q4"]);
        let big = &scan.models[0];
        assert_eq!(big.parts, 2);
        assert_eq!(big.size_bytes, 2 * llama("Big").len() as u64);
        assert!(scan.models[1].mmproj.is_some());
        assert!(scan.models[2].mmproj.is_none());
        assert_eq!(scan.skipped.len(), 1);
        assert!(scan.skipped[0].path.ends_with("broken.gguf"));
    }

    #[test]
    fn incomplete_split_sets_and_duplicate_ids_are_skipped() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        fs::write(first.path().join("m.gguf"), llama("a")).unwrap();
        fs::write(second.path().join("m.gguf"), llama("b")).unwrap();
        fs::write(second.path().join("part-00001-of-00002.gguf"), llama("c")).unwrap();
        let scan = scan(&[first.path().to_path_buf(), second.path().to_path_buf()]);
        assert_eq!(scan.models.len(), 1);
        assert_eq!(scan.models[0].info.name.as_deref(), Some("a"));
        assert_eq!(scan.skipped.len(), 2);
    }

    #[test]
    fn hugging_face_folders_are_found_and_not_searched_further() {
        let root = tempfile::tempdir().unwrap();
        let model = root.path().join("Qwen3-8B");
        fs::create_dir_all(model.join("nested")).unwrap();
        fs::write(
            model.join("config.json"),
            r#"{"architectures": ["Qwen3ForCausalLM"], "num_hidden_layers": 36, "hidden_size": 4096,
                "num_attention_heads": 32, "num_key_value_heads": 8, "head_dim": 128,
                "max_position_embeddings": 40960}"#,
        )
        .unwrap();
        fs::write(model.join("model-00001-of-00002.safetensors"), vec![0; 300]).unwrap();
        fs::write(model.join("model-00002-of-00002.safetensors"), vec![0; 200]).unwrap();
        fs::write(model.join("nested/inner.gguf"), llama("ignored")).unwrap();
        let scan = scan(&[root.path().to_path_buf()]);
        assert_eq!(scan.models.len(), 1);
        let found = &scan.models[0];
        assert_eq!(
            (found.id.as_str(), found.format),
            ("Qwen3-8B", Format::Safetensors)
        );
        assert_eq!(found.size_bytes, 500);
        assert_eq!(found.info.architecture.as_deref(), Some("Qwen3ForCausalLM"));
        assert_eq!(found.info.kv_bytes_per_token(2.0), Some(36 * 8 * 256 * 2));
    }

    #[test]
    fn lm_studio_and_download_layouts_name_the_repository() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("lmstudio-community/Qwen3.5-9B-GGUF");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("Qwen3.5-9B-Q8_0.gguf"), llama("Qwen")).unwrap();
        fs::write(repo.join("mmproj-Qwen3.5-9B-BF16.gguf"), llama("proj")).unwrap();
        let deep = root.path().join("a/b/unsloth/Qwen3-8B-GGUF");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("Qwen3-8B-Q4_K_M.gguf"), llama("Qwen3")).unwrap();
        let single = root.path().join("loose");
        fs::create_dir_all(&single).unwrap();
        fs::write(single.join("tiny.gguf"), llama("tiny")).unwrap();
        fs::write(root.path().join("top.gguf"), llama("top")).unwrap();

        let scan = scan(&[root.path().to_path_buf()]);
        let found: Vec<_> = scan
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.repo.as_deref(), m.mmproj.is_some()))
            .collect();
        assert_eq!(
            found,
            [
                ("Qwen3-8B-Q4_K_M", Some("unsloth/Qwen3-8B-GGUF"), false),
                (
                    "Qwen3.5-9B-Q8_0",
                    Some("lmstudio-community/Qwen3.5-9B-GGUF"),
                    true
                ),
                ("tiny", Some("loose"), false),
                ("top", None, false),
            ]
        );
        assert!(scan.skipped.is_empty(), "{:?}", scan.skipped);
    }

    #[test]
    fn hugging_face_cache_is_scanned_at_the_main_revision() {
        let root = tempfile::tempdir().unwrap();
        let cache = root
            .path()
            .join("hub/models--unsloth--Qwen3-4B-Instruct-2507-GGUF");
        let main = "a".repeat(40);
        let old = "b".repeat(40);
        fs::create_dir_all(cache.join("refs")).unwrap();
        fs::write(cache.join("refs/main"), &main).unwrap();
        for revision in [&main, &old] {
            let snapshot = cache.join("snapshots").join(revision);
            fs::create_dir_all(&snapshot).unwrap();
            fs::write(
                snapshot.join("Qwen3-4B-Instruct-2507-Q4_K_M.gguf"),
                llama(revision),
            )
            .unwrap();
        }
        // Placeholders for files the Hub does not have, and the blob store, are not models.
        let missing = cache.join(".no_exist").join(&main);
        fs::create_dir_all(&missing).unwrap();
        fs::write(missing.join("other-Q8_0.gguf"), "").unwrap();
        fs::create_dir_all(cache.join("blobs")).unwrap();
        fs::write(cache.join("blobs/0123abcd"), llama("blob")).unwrap();
        // A safetensors model in the cache is named after its repository, not its revision.
        let st = root.path().join("hub/models--Qwen--Qwen3-0.6B");
        let revision = "c".repeat(40);
        let snapshot = st.join("snapshots").join(&revision);
        fs::create_dir_all(&snapshot).unwrap();
        fs::write(
            snapshot.join("config.json"),
            r#"{"architectures": ["Qwen3ForCausalLM"], "num_hidden_layers": 28}"#,
        )
        .unwrap();
        fs::write(snapshot.join("model.safetensors"), vec![0; 64]).unwrap();

        let scan = scan(&[root.path().join("hub")]);
        let found: Vec<_> = scan
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.repo.as_deref(), m.format))
            .collect();
        assert_eq!(
            found,
            [
                ("Qwen3-0.6B", Some("Qwen/Qwen3-0.6B"), Format::Safetensors),
                (
                    "Qwen3-4B-Instruct-2507-Q4_K_M",
                    Some("unsloth/Qwen3-4B-Instruct-2507-GGUF"),
                    Format::Gguf
                ),
            ]
        );
        assert_eq!(scan.models[1].info.name.as_deref(), Some(main.as_str()));
        assert!(scan.skipped.is_empty(), "{:?}", scan.skipped);
        assert_eq!(hf_cache_repo("models--a--b--c").as_deref(), Some("a/b--c"));
        assert_eq!(hf_cache_repo("datasets--a--b"), None);
    }

    #[test]
    fn image_models_and_diffusers_pipelines_are_left_out_with_a_reason() {
        use crate::gguf::fixture::{V, header};
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("flux-2-klein-9b-Q8_0.gguf"),
            header(&[
                ("general.architecture", V::Str("flux")),
                ("general.file_type", V::U32(7)),
            ]),
        )
        .unwrap();
        let pipeline = root.path().join("FLUX.2-klein-9B");
        fs::create_dir_all(pipeline.join("text_encoder")).unwrap();
        fs::write(pipeline.join("model_index.json"), "{}").unwrap();
        fs::write(
            pipeline.join("text_encoder/config.json"),
            r#"{"architectures": ["Qwen3ForCausalLM"]}"#,
        )
        .unwrap();
        fs::write(pipeline.join("text_encoder/model.safetensors"), vec![0; 8]).unwrap();
        fs::write(root.path().join("chat.gguf"), llama("chat")).unwrap();

        let scan = scan(&[root.path().to_path_buf()]);
        let ids: Vec<_> = scan.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["chat"]);
        let reasons: Vec<_> = scan.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert_eq!(reasons.len(), 2, "{reasons:?}");
        assert!(reasons.iter().any(|r| r.contains("image model")));
        assert!(reasons.iter().any(|r| r.contains("diffusers")));
    }

    #[test]
    fn each_model_gets_the_projector_that_matches_its_name() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "gemma-3-4b-it-Q4_K_M",
            "qwen2.5-vl-7b-Q4_K_M",
            "mmproj-gemma-3-4b-it-f16",
            "mmproj-qwen2.5-vl-7b-f16",
        ] {
            fs::write(root.path().join(format!("{name}.gguf")), llama(name)).unwrap();
        }
        let scan = scan(&[root.path().to_path_buf()]);
        let pairs: Vec<_> = scan
            .models
            .iter()
            .map(|m| {
                let proj = m.mmproj.as_ref().unwrap().file_stem().unwrap();
                (m.id.clone(), proj.to_string_lossy().into_owned())
            })
            .collect();
        assert_eq!(
            pairs,
            [
                (
                    "gemma-3-4b-it-Q4_K_M".to_string(),
                    "mmproj-gemma-3-4b-it-f16".to_string()
                ),
                (
                    "qwen2.5-vl-7b-Q4_K_M".to_string(),
                    "mmproj-qwen2.5-vl-7b-f16".to_string()
                ),
            ]
        );
    }

    #[test]
    fn split_names_are_recognised() {
        assert_eq!(split_part("m-00002-of-00004"), Some(("m", 2, 4)));
        assert_eq!(split_part("model-q4"), None);
        assert_eq!(split_part("a-1-of-2"), None);
    }
}
