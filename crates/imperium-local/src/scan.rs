//! Finds GGUF models in the configured folders.

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

/// Scans `folders` recursively. Symbolic links to folders are not followed, so loops are
/// impossible; when two files share an id, the first folder listed wins.
pub fn scan(folders: &[PathBuf]) -> Scan {
    let mut scan = Scan::default();
    let mut ids = BTreeMap::new();
    for folder in folders {
        let mut files = Vec::new();
        let mut hf_dirs = Vec::new();
        collect(folder, &mut files, &mut hf_dirs, &mut scan.skipped);
        files.sort();
        hf_dirs.sort();
        for dir_files in group_by_dir(&files) {
            add_dir(&dir_files, &mut scan, &mut ids);
        }
        for dir in hf_dirs {
            add_hf_dir(&dir, &mut scan, &mut ids);
        }
    }
    scan.models.sort_by(|a, b| a.id.cmp(&b.id));
    scan
}

/// Gathers GGUF files, and Hugging Face model folders (not searched further), under `dir`.
fn collect(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    hf_dirs: &mut Vec<PathBuf>,
    skipped: &mut Vec<Skipped>,
) {
    if is_hf_dir(dir) {
        hf_dirs.push(dir.to_path_buf());
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
            collect(&path, files, hf_dirs, skipped);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
        {
            files.push(path);
        }
    }
}

fn group_by_dir(files: &[PathBuf]) -> Vec<Vec<&PathBuf>> {
    let mut dirs: BTreeMap<&Path, Vec<&PathBuf>> = BTreeMap::new();
    for file in files {
        dirs.entry(file.parent().unwrap_or(Path::new("")))
            .or_default()
            .push(file);
    }
    dirs.into_values().collect()
}

fn add_dir(files: &[&PathBuf], scan: &mut Scan, ids: &mut BTreeMap<String, PathBuf>) {
    let stem = |path: &Path| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let mmproj = files
        .iter()
        .find(|path| stem(path).to_ascii_lowercase().contains("mmproj"))
        .map(|path| path.to_path_buf());
    for path in files {
        let name = stem(path);
        if name.to_ascii_lowercase().contains("mmproj") {
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
                scan.skipped.push(skip(error.to_string()));
                continue;
            }
        };
        if let Some(first) = ids.get(&id) {
            scan.skipped.push(skip(format!(
                "id {id:?} is already used by {}",
                first.display()
            )));
            continue;
        }
        ids.insert(id.clone(), path.to_path_buf());
        scan.models.push(LocalModel {
            id,
            path: path.to_path_buf(),
            parts,
            size_bytes,
            info,
            mmproj: mmproj.clone(),
            format: Format::Gguf,
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

/// Adds a Hugging Face model folder, reading its shape from `config.json`.
fn add_hf_dir(dir: &Path, scan: &mut Scan, ids: &mut BTreeMap<String, PathBuf>) {
    let skip = |reason: String| Skipped {
        path: dir.to_path_buf(),
        reason,
    };
    let id = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
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
    fn split_names_are_recognised() {
        assert_eq!(split_part("m-00002-of-00004"), Some(("m", 2, 4)));
        assert_eq!(split_part("model-q4"), None);
        assert_eq!(split_part("a-1-of-2"), None);
    }
}
