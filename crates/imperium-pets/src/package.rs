//! Pixel Pets v3 packages: `pet.json`, the atlas, and per-animation strips, with the 22
//! standard animations, custom ones, and fallback resolution as `docs/pet-format-v3.md` in the
//! Pixel Pets repository defines them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// Largest strip PNG a package may use.
pub const MAX_STRIP_BYTES: u64 = 96 * 1024;

/// One animation's timing and behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Animation {
    pub name: String,
    pub row: u32,
    pub frames: u32,
    /// One duration in ms per frame.
    pub durations: Vec<u32>,
    pub looping: bool,
    /// 0 (highest, user interaction) to 6 (ambient).
    pub priority: u8,
    pub min_ms: u32,
    pub next: Option<String>,
    pub fallback: Option<String>,
}

fn repeat(value: u32, count: usize, last: Option<u32>) -> Vec<u32> {
    let mut durations = vec![value; count];
    if let Some(last) = last {
        durations.push(last);
    }
    durations
}

/// The 22 standard animations of format v3 (the look poses are not timed animations).
pub fn standard() -> Vec<Animation> {
    let a = |name: &str,
             row,
             frames,
             durations: Vec<u32>,
             looping,
             priority,
             fallback: Option<&str>| Animation {
        name: name.into(),
        row,
        frames,
        durations,
        looping,
        priority,
        min_ms: 600,
        next: None,
        fallback: fallback.map(str::to_string),
    };
    vec![
        a(
            "idle",
            0,
            6,
            vec![280, 110, 110, 140, 140, 320],
            true,
            6,
            None,
        ),
        a(
            "walk-right",
            1,
            8,
            repeat(120, 7, Some(220)),
            true,
            6,
            Some("idle"),
        ),
        a(
            "walk-left",
            2,
            8,
            repeat(120, 7, Some(220)),
            true,
            6,
            Some("idle"),
        ),
        a(
            "waving",
            3,
            4,
            repeat(140, 3, Some(280)),
            false,
            5,
            Some("idle"),
        ),
        a(
            "jumping",
            4,
            5,
            repeat(140, 4, Some(280)),
            false,
            3,
            Some("idle"),
        ),
        a(
            "failed",
            5,
            8,
            repeat(140, 7, Some(240)),
            false,
            2,
            Some("idle"),
        ),
        a(
            "waiting",
            6,
            6,
            repeat(150, 5, Some(260)),
            true,
            1,
            Some("idle"),
        ),
        a(
            "working",
            7,
            6,
            repeat(120, 5, Some(220)),
            true,
            4,
            Some("idle"),
        ),
        a(
            "review",
            8,
            6,
            repeat(150, 5, Some(280)),
            true,
            4,
            Some("working"),
        ),
        a(
            "sleeping",
            11,
            6,
            repeat(400, 6, None),
            true,
            6,
            Some("idle"),
        ),
        a(
            "waking",
            12,
            6,
            repeat(120, 5, Some(200)),
            false,
            5,
            Some("idle"),
        ),
        a(
            "summoning",
            13,
            6,
            repeat(120, 5, Some(240)),
            false,
            4,
            Some("waving"),
        ),
        a(
            "celebrating",
            14,
            8,
            repeat(110, 7, Some(300)),
            false,
            3,
            Some("jumping"),
        ),
        a(
            "confused",
            15,
            6,
            repeat(150, 5, Some(300)),
            false,
            2,
            Some("failed"),
        ),
        a(
            "eating",
            16,
            6,
            repeat(140, 6, None),
            true,
            4,
            Some("working"),
        ),
        a(
            "fidget",
            17,
            6,
            repeat(160, 5, Some(300)),
            false,
            6,
            Some("idle"),
        ),
        a(
            "pop-out",
            18,
            6,
            repeat(90, 5, Some(160)),
            false,
            5,
            Some("jumping"),
        ),
        a(
            "duck-in",
            19,
            6,
            repeat(90, 5, Some(160)),
            false,
            5,
            Some("idle"),
        ),
        a(
            "alert",
            20,
            4,
            repeat(120, 4, None),
            true,
            1,
            Some("waiting"),
        ),
        a(
            "petted",
            21,
            6,
            repeat(120, 5, Some(260)),
            false,
            0,
            Some("waving"),
        ),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pet {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub cell_width: u32,
    pub cell_height: u32,
    pub dir: PathBuf,
    /// Every animation the pet defines (standard and custom), keyed by name.
    pub animations: BTreeMap<String, Animation>,
    /// Animations whose strips are present and well-formed.
    pub drawn: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PetError {
    #[error("{0}: {1}")]
    Invalid(String, String),
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    license: String,
    sprite_version_number: u32,
    #[serde(default = "default_strips")]
    strips_path: String,
    cell: Cell,
    #[serde(default)]
    animations: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct Cell {
    width: u32,
    height: u32,
}

fn default_strips() -> String {
    "strips".into()
}

impl Pet {
    /// Loads and checks the package in `dir`.
    pub fn load(dir: &Path) -> Result<Self, PetError> {
        let invalid = |reason: String| PetError::Invalid(dir.display().to_string(), reason);
        let text = std::fs::read_to_string(dir.join("pet.json"))
            .map_err(|error| PetError::Io(dir.join("pet.json").display().to_string(), error))?;
        let manifest: Manifest =
            serde_json::from_str(&text).map_err(|error| invalid(format!("pet.json: {error}")))?;
        if manifest.sprite_version_number != 3 {
            return Err(invalid(format!(
                "spriteVersionNumber {} is not 3; import Codex pets with the Pixel Pets importer",
                manifest.sprite_version_number
            )));
        }
        let folder = dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if manifest.id != folder {
            return Err(invalid(format!(
                "id {:?} must equal the folder name",
                manifest.id
            )));
        }
        for size in [manifest.cell.width, manifest.cell.height] {
            if !(16..=128).contains(&size) {
                return Err(invalid(format!("cell size {size} is outside 16-128")));
            }
        }
        let mut animations: BTreeMap<String, Animation> = standard()
            .into_iter()
            .map(|animation| (animation.name.clone(), animation))
            .collect();
        for (name, overrides) in &manifest.animations {
            let animation = override_animation(animations.get(name).cloned(), name, overrides)
                .map_err(|reason| invalid(format!("animations.{name}: {reason}")))?;
            animations.insert(name.clone(), animation);
        }
        let strips = dir.join(&manifest.strips_path);
        let drawn: Vec<String> = animations
            .values()
            .filter(|animation| {
                strip_ok(
                    &strips.join(format!("{}.png", animation.name)),
                    animation.frames * manifest.cell.width,
                    manifest.cell.height,
                )
            })
            .map(|animation| animation.name.clone())
            .collect();
        if !drawn.iter().any(|name| name == "idle") {
            return Err(invalid(
                "idle must be drawn (strips/idle.png); run the packager".into(),
            ));
        }
        Ok(Pet {
            display_name: manifest.display_name.unwrap_or_else(|| manifest.id.clone()),
            id: manifest.id,
            description: manifest.description,
            author: manifest.author,
            license: manifest.license,
            cell_width: manifest.cell.width,
            cell_height: manifest.cell.height,
            dir: dir.to_path_buf(),
            animations,
            drawn,
        })
    }

    /// The animation actually shown for `name`: itself if drawn, else its fallback chain.
    pub fn resolve(&self, name: &str) -> &Animation {
        let mut current = name;
        for _ in 0..self.animations.len() {
            match self.animations.get(current) {
                Some(animation) if self.drawn.iter().any(|drawn| drawn == current) => {
                    return animation;
                }
                Some(Animation {
                    fallback: Some(fallback),
                    ..
                }) => current = fallback,
                _ => break,
            }
        }
        &self.animations["idle"]
    }

    /// The strip PNG for an animation the pet draws.
    pub fn strip(&self, name: &str) -> Option<PathBuf> {
        self.drawn
            .iter()
            .any(|drawn| drawn == name)
            .then(|| self.dir.join("strips").join(format!("{name}.png")))
    }
}

/// A standard animation with `pet.json` overrides applied, or a custom one.
fn override_animation(
    base: Option<Animation>,
    name: &str,
    value: &Value,
) -> Result<Animation, String> {
    let get_u32 = |key: &str| value.get(key).and_then(Value::as_u64).map(|n| n as u32);
    let custom = base.is_none();
    let mut animation = base.unwrap_or(Animation {
        name: name.to_string(),
        row: 0,
        frames: 0,
        durations: Vec::new(),
        looping: false,
        priority: 4,
        min_ms: 600,
        next: None,
        fallback: None,
    });
    if custom {
        let row = get_u32("row").ok_or("a custom animation needs row")?;
        if row < 22 {
            return Err("custom animations use rows 22 and below".into());
        }
        animation.row = row;
        if value.get("fallback").is_none()
            || value.get("durations").is_none()
            || value.get("frames").is_none()
        {
            return Err("a custom animation needs frames, durations, and fallback".into());
        }
    } else if let Some(row) = get_u32("row") {
        animation.row = row;
    }
    if let Some(frames) = get_u32("frames") {
        if !(1..=16).contains(&frames) {
            return Err("frames must be 1-16".into());
        }
        animation.frames = frames;
    }
    match value.get("durations") {
        Some(Value::Number(n)) => {
            animation.durations = vec![n.as_u64().unwrap_or(150) as u32; animation.frames as usize];
        }
        Some(Value::Array(list)) => {
            animation.durations = list
                .iter()
                .map(|n| {
                    n.as_u64()
                        .map(|n| n as u32)
                        .ok_or("durations must be numbers")
                })
                .collect::<Result<_, _>>()?;
        }
        _ => {}
    }
    if animation.durations.len() != animation.frames as usize {
        animation.durations =
            vec![animation.durations.first().copied().unwrap_or(150); animation.frames as usize];
    }
    if let Some(looping) = value.get("loop").and_then(Value::as_bool) {
        animation.looping = looping;
    }
    match value.get("priority") {
        Some(Value::Number(n)) => animation.priority = n.as_u64().unwrap_or(4).min(6) as u8,
        Some(Value::String(text)) => {
            animation.priority = text
                .strip_prefix('P')
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| *n <= 6)
                .ok_or("priority must be P0-P6")?;
        }
        _ => {}
    }
    if let Some(min_ms) = get_u32("minMs") {
        animation.min_ms = min_ms;
    }
    if let Some(next) = value.get("next").and_then(Value::as_str) {
        animation.next = Some(next.to_string());
    }
    if let Some(fallback) = value.get("fallback").and_then(Value::as_str) {
        animation.fallback = Some(fallback.to_string());
    }
    Ok(animation)
}

/// Whether a strip exists, is within the size limit, and is `width` x `height` pixels.
fn strip_ok(path: &Path, width: u32, height: u32) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() > MAX_STRIP_BYTES {
        return false;
    }
    png_size(path) == Some((width, height))
}

/// A PNG's width and height from its IHDR chunk.
pub fn png_size(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    let mut header = [0u8; 24];
    std::fs::File::open(path)
        .ok()?
        .read_exact(&mut header)
        .ok()?;
    if &header[..8] != b"\x89PNG\r\n\x1a\n" || &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    Some((width, height))
}

/// Every loadable pet in `folders` (each a folder of pet folders); problems are reported.
pub fn discover(folders: &[PathBuf]) -> (Vec<Pet>, Vec<String>) {
    let mut pets: Vec<Pet> = Vec::new();
    let mut problems = Vec::new();
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join("pet.json").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            match Pet::load(&dir) {
                Ok(pet) if pets.iter().any(|other| other.id == pet.id) => {}
                Ok(pet) => pets.push(pet),
                Err(error) => problems.push(error.to_string()),
            }
        }
    }
    (pets, problems)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The Pixel Clawd pet bundled in the repository.
    pub fn bundled() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pets/pixel-clawd")
    }

    #[test]
    fn the_bundled_pet_loads_with_every_standard_animation() {
        let pet = Pet::load(&bundled().canonicalize().unwrap()).unwrap();
        assert_eq!(pet.id, "pixel-clawd");
        assert_eq!((pet.cell_width, pet.cell_height), (32, 32));
        assert_eq!(pet.animations.len(), 20);
        assert!(pet.drawn.contains(&"idle".to_string()));
        assert_eq!(pet.resolve("working").name, "working");
        assert!(pet.strip("idle").unwrap().is_file());
    }

    #[test]
    fn missing_strips_fall_back_and_custom_animations_are_checked() {
        let dir = tempfile::tempdir().unwrap();
        let pet_dir = dir.path().join("mini");
        std::fs::create_dir_all(pet_dir.join("strips")).unwrap();
        std::fs::copy(
            bundled().join("strips/idle.png"),
            pet_dir.join("strips/idle.png"),
        )
        .unwrap();
        std::fs::write(
            pet_dir.join("pet.json"),
            r#"{"id": "mini", "spriteVersionNumber": 3, "cell": {"width": 32, "height": 32},
                "animations": {"working": {"minMs": 900, "priority": "P3"},
                               "dance": {"row": 22, "frames": 4, "durations": 100, "fallback": "jumping", "loop": true}}}"#,
        )
        .unwrap();
        let pet = Pet::load(&pet_dir).unwrap();
        assert_eq!(pet.drawn, ["idle"]);
        // review -> working -> idle; celebrating -> jumping -> idle.
        assert_eq!(pet.resolve("review").name, "idle");
        assert_eq!(pet.animations["working"].min_ms, 900);
        assert_eq!(pet.animations["working"].priority, 3);
        assert_eq!(pet.animations["dance"].durations, [100; 4]);
        assert!(pet.animations["dance"].looping);

        std::fs::write(
            pet_dir.join("pet.json"),
            r#"{"id": "mini", "spriteVersionNumber": 3, "cell": {"width": 32, "height": 32},
                "animations": {"dance": {"row": 5, "frames": 4, "durations": 100, "fallback": "idle"}}}"#,
        )
        .unwrap();
        assert!(
            Pet::load(&pet_dir)
                .unwrap_err()
                .to_string()
                .contains("rows 22")
        );
        std::fs::write(
            pet_dir.join("pet.json"),
            r#"{"id": "other", "spriteVersionNumber": 3, "cell": {"width": 32, "height": 32}}"#,
        )
        .unwrap();
        assert!(
            Pet::load(&pet_dir)
                .unwrap_err()
                .to_string()
                .contains("folder name")
        );
    }
}
