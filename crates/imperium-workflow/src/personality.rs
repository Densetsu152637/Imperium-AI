//! Root personalities (docs/08-agent-policy.md#personalities): Markdown files that set the tone
//! of the root agent's replies to the user. Subagents never get one.
//!
//! A personality id is a file stem, such as `marcus_aurelius`. It is looked up, first match
//! wins, in the user's local folder (`<config>/personalities/<id>.md`), then in each workflow
//! pack's `personalities/` folder, then in the built-in copy (the bundled pack, and failing that
//! the default personality compiled into the binary), so the default always resolves.

use std::path::{Path, PathBuf};

/// The personality used when `policy.personality` is unset, or names one that is not found.
pub const DEFAULT_PERSONALITY: &str = "marcus_aurelius";

/// The default personality's text, so it resolves without any pack on disk.
const BUILT_IN_DEFAULT: &str =
    include_str!("../../../policy/agent-custom-workflow/personalities/marcus_aurelius.md");

/// The folder, inside a pack and inside the config folder, that holds personalities.
pub const FOLDER: &str = "personalities";

/// Where a personality was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// `<config>/personalities/`, written by the user.
    Local,
    /// A workflow pack's `personalities/` folder; the pack's folder name.
    Pack(String),
    /// The bundled pack, or the copy compiled into the binary.
    BuiltIn,
}

impl Origin {
    /// `local`, `pack`, or `builtin`.
    pub fn kind(&self) -> &'static str {
        match self {
            Origin::Local => "local",
            Origin::Pack(_) => "pack",
            Origin::BuiltIn => "builtin",
        }
    }
}

/// A personality as listed: its id, display name, and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalityInfo {
    pub id: String,
    /// The file's first heading, without a `Personality Profile:` prefix; the id otherwise.
    pub name: String,
    pub origin: Origin,
    /// A local personality that hides a pack or built-in one with the same id.
    pub overrides: bool,
}

/// A loaded personality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Personality {
    pub info: PersonalityInfo,
    pub text: String,
}

impl Personality {
    /// The block appended to the root agent's system prompt, after its reply rules: the
    /// personality changes tone, never those rules or what is delivered.
    pub fn instructions(&self) -> String {
        format!(
            "Personality: Imperium has selected {name} as your voice with the user; it is \
             described below, so do not look for or load another personality from the workflow. \
             It sets only the tone of your replies to the user. The reply rules above still \
             hold, and code, commands, paths, commit messages, pull requests, documentation, and \
             tool inputs stay plain and professional. State a warning about a destructive action \
             or a security risk plainly first. Never pass the personality to subagents or put it \
             in a brief.\n\n<personality name=\"{name}\">\n{text}\n</personality>",
            name = self.info.name,
            text = self.text.trim(),
        )
    }
}

/// The result of resolving a `policy.personality` setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// `None` when personalities are turned off.
    pub personality: Option<Personality>,
    /// Why the setting was not used as written, such as an id that was not found.
    pub warning: Option<String>,
}

/// Whether a `policy.personality` value turns personalities off: empty or `none`.
pub fn is_off(setting: &str) -> bool {
    let setting = setting.trim();
    setting.is_empty() || setting.eq_ignore_ascii_case("none")
}

/// Whether `id` can name a personality file: letters, digits, `_`, and `-` only, so it never
/// reaches outside a personalities folder.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The places personalities are looked up, in precedence order.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// The user's config folder; its `personalities/` subfolder holds local personalities.
    pub config_dir: Option<PathBuf>,
    /// Workflow pack folders, in order; each may have a `personalities/` subfolder.
    pub packs: Vec<PathBuf>,
    /// The bundled pack, when it is on disk.
    pub built_in: Option<PathBuf>,
}

impl Sources {
    /// Every personality folder with the origin it gives, in precedence order.
    fn folders(&self) -> Vec<(PathBuf, Origin)> {
        let mut folders = Vec::new();
        if let Some(config) = &self.config_dir {
            folders.push((config.join(FOLDER), Origin::Local));
        }
        for pack in &self.packs {
            let name = pack
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "workflow".into());
            folders.push((pack.join(FOLDER), Origin::Pack(name)));
        }
        if let Some(built_in) = &self.built_in {
            folders.push((built_in.join(FOLDER), Origin::BuiltIn));
        }
        folders
    }

    /// Every personality that can be selected, each id once (the one that wins), sorted by
    /// name. The default is always among them.
    pub fn list(&self) -> Vec<PersonalityInfo> {
        let mut found: Vec<PersonalityInfo> = Vec::new();
        for (folder, origin) in self.folders() {
            for id in ids_in(&folder) {
                if let Some(winner) = found.iter_mut().find(|info| info.id == id) {
                    if winner.origin == Origin::Local {
                        winner.overrides = true;
                    }
                    continue;
                }
                let name = std::fs::read_to_string(folder.join(format!("{id}.md")))
                    .map(|text| display_name(&text, &id))
                    .unwrap_or_else(|_| id.clone());
                found.push(PersonalityInfo {
                    id,
                    name,
                    origin: origin.clone(),
                    overrides: false,
                });
            }
        }
        match found.iter_mut().find(|info| info.id == DEFAULT_PERSONALITY) {
            // A local file overrides the compiled-in default too.
            Some(info) if info.origin == Origin::Local => info.overrides = true,
            Some(_) => {}
            None => found.push(built_in_default().info),
        }
        found.sort_by_key(|info| info.name.to_lowercase());
        found
    }

    /// The personality with this id, from the first folder that has it.
    pub fn find(&self, id: &str) -> Option<Personality> {
        if !is_valid_id(id) {
            return None;
        }
        let mut found: Option<Personality> = None;
        for (folder, origin) in self.folders() {
            let Ok(text) = std::fs::read_to_string(folder.join(format!("{id}.md"))) else {
                continue;
            };
            match &mut found {
                Some(winner) => {
                    if winner.info.origin == Origin::Local {
                        winner.info.overrides = true;
                    }
                    break;
                }
                None => {
                    found = Some(Personality {
                        info: PersonalityInfo {
                            id: id.to_string(),
                            name: display_name(&text, id),
                            origin,
                            overrides: false,
                        },
                        text,
                    });
                }
            }
        }
        match found {
            Some(personality) if personality.info.origin == Origin::Local => {
                // A local file overrides the compiled-in default too.
                let mut personality = personality;
                if id == DEFAULT_PERSONALITY {
                    personality.info.overrides = true;
                }
                Some(personality)
            }
            None if id == DEFAULT_PERSONALITY => Some(built_in_default()),
            other => other,
        }
    }

    /// Resolves a `policy.personality` value: off for empty or `none`; otherwise that
    /// personality, or the default with a warning when it is not found.
    pub fn select(&self, setting: &str) -> Selection {
        if is_off(setting) {
            return Selection {
                personality: None,
                warning: None,
            };
        }
        let id = setting.trim();
        if let Some(personality) = self.find(id) {
            return Selection {
                personality: Some(personality),
                warning: None,
            };
        }
        let fallback = self
            .find(DEFAULT_PERSONALITY)
            .unwrap_or_else(built_in_default);
        Selection {
            warning: Some(format!(
                "personality \"{id}\" was not found in {}/ or a workflow pack; using {}",
                FOLDER, fallback.info.name
            )),
            personality: Some(fallback),
        }
    }
}

/// The default personality as compiled into the binary.
fn built_in_default() -> Personality {
    Personality {
        info: PersonalityInfo {
            id: DEFAULT_PERSONALITY.into(),
            name: display_name(BUILT_IN_DEFAULT, DEFAULT_PERSONALITY),
            origin: Origin::BuiltIn,
            overrides: false,
        },
        text: BUILT_IN_DEFAULT.into(),
    }
}

/// The ids of the personality files in `folder`: `*.md` files with a valid stem, except the
/// folder's README.
fn ids_in(folder: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "md") {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            (is_valid_id(&stem) && !stem.eq_ignore_ascii_case("readme")).then_some(stem)
        })
        .collect();
    ids.sort();
    ids
}

/// A personality's display name: its first `#` heading without a `Personality Profile:` or
/// `Personality:` prefix, else the id.
fn display_name(text: &str, id: &str) -> String {
    text.lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|heading| {
            let heading = heading.trim();
            heading
                .strip_prefix("Personality Profile:")
                .or_else(|| heading.strip_prefix("Personality:"))
                .unwrap_or(heading)
                .trim()
                .to_string()
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, id: &str, text: &str) {
        std::fs::create_dir_all(dir.join(FOLDER)).unwrap();
        std::fs::write(dir.join(FOLDER).join(format!("{id}.md")), text).unwrap();
    }

    #[test]
    fn with_no_sources_the_default_is_marcus_aurelius() {
        let selection = Sources::default().select(DEFAULT_PERSONALITY);
        let personality = selection.personality.unwrap();
        assert_eq!(personality.info.id, "marcus_aurelius");
        assert_eq!(personality.info.name, "Marcus Aurelius");
        assert_eq!(personality.info.origin, Origin::BuiltIn);
        assert!(personality.text.contains("Stoic"));
        assert!(selection.warning.is_none());
        let instructions = personality.instructions();
        assert!(instructions.contains("<personality name=\"Marcus Aurelius\">"));
        assert!(instructions.contains("reply rules above still hold"));
        let list = Sources::default().list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, DEFAULT_PERSONALITY);
    }

    #[test]
    fn a_selected_pack_personality_resolves() {
        let pack = tempfile::tempdir().unwrap();
        write(
            pack.path(),
            "luxion",
            "# Personality Profile: Luxion\n\nDeadpan.\n",
        );
        let sources = Sources {
            packs: vec![pack.path().to_path_buf()],
            ..Sources::default()
        };
        let personality = sources.select("luxion").personality.unwrap();
        assert_eq!(personality.info.name, "Luxion");
        assert!(matches!(personality.info.origin, Origin::Pack(_)));
        assert!(personality.text.contains("Deadpan"));
    }

    #[test]
    fn a_local_personality_overrides_pack_and_built_in_ones() {
        let config = tempfile::tempdir().unwrap();
        let pack = tempfile::tempdir().unwrap();
        write(pack.path(), "luxion", "# Luxion\n\nPack voice.\n");
        write(config.path(), "luxion", "# My Luxion\n\nLocal voice.\n");
        write(
            config.path(),
            "marcus_aurelius",
            "# Marcus\n\nMy own Marcus.\n",
        );
        write(config.path(), "mine", "No heading.\n");
        let sources = Sources {
            config_dir: Some(config.path().to_path_buf()),
            packs: vec![pack.path().to_path_buf()],
            built_in: None,
        };
        let luxion = sources.find("luxion").unwrap();
        assert_eq!(luxion.info.origin, Origin::Local);
        assert!(luxion.info.overrides);
        assert!(luxion.text.contains("Local voice"));
        let marcus = sources.find(DEFAULT_PERSONALITY).unwrap();
        assert!(marcus.text.contains("My own Marcus") && marcus.info.overrides);

        let list = sources.list();
        let ids: Vec<_> = list.iter().map(|info| info.id.as_str()).collect();
        assert_eq!(ids, ["marcus_aurelius", "mine", "luxion"]);
        assert!(list.iter().all(|info| info.origin == Origin::Local));
        assert!(list[2].overrides && !list[1].overrides);
        assert_eq!(list[1].name, "mine");
    }

    #[test]
    fn none_or_empty_turns_personalities_off() {
        for setting in ["none", "None", "", "  "] {
            let selection = Sources::default().select(setting);
            assert!(selection.personality.is_none(), "{setting:?}");
            assert!(selection.warning.is_none());
        }
    }

    #[test]
    fn an_unknown_or_unsafe_id_warns_and_falls_back_to_the_default() {
        for setting in ["nobody", "../secret", "a/b"] {
            let selection = Sources::default().select(setting);
            assert_eq!(
                selection.personality.unwrap().info.id,
                DEFAULT_PERSONALITY,
                "{setting}"
            );
            let warning = selection.warning.unwrap();
            assert!(warning.contains(setting) && warning.contains("Marcus Aurelius"));
        }
    }

    #[test]
    fn the_bundled_pack_lists_marcus_aurelius_as_built_in() {
        let bundled =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policy/agent-custom-workflow");
        let sources = Sources {
            built_in: Some(bundled),
            ..Sources::default()
        };
        let list = sources.list();
        assert!(list.len() > 10);
        let marcus = list
            .iter()
            .find(|info| info.id == DEFAULT_PERSONALITY)
            .unwrap();
        assert_eq!(marcus.name, "Marcus Aurelius");
        assert_eq!(marcus.origin, Origin::BuiltIn);
        assert!(
            !list
                .iter()
                .any(|info| info.id.eq_ignore_ascii_case("readme"))
        );
    }
}
