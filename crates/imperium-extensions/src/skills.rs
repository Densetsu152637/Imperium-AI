//! Agent Skills: folders with a `SKILL.md` (name and description in front matter, instructions
//! in the body) and optional resource files. Agents see only each skill's name, description,
//! and trigger hint (front matter `when_to_use`) up front, and load the instructions of the ones
//! they need with the `load_skill` tool; an eager skill's instructions are in the prompt from
//! the start.

use std::path::{Component, Path, PathBuf};

use imperium_api::markdown::front_matter;

/// Longest description shown in the skill index.
const DESCRIPTION_CHARS: usize = 300;
/// Longest trigger hint shown in the skill index.
const HINT_CHARS: usize = 150;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The folder holding `SKILL.md` and its resources.
    pub dir: PathBuf,
    /// Where it came from, such as `user`, `project`, or `plugin:<name>`.
    pub origin: String,
    /// When to use it (front matter `when_to_use`), shown in the index after the description.
    pub hint: Option<String>,
    /// Its instructions are in the prompt from the start rather than loaded on use.
    pub eager: bool,
}

/// Finds skills in each `(folder, origin)`: every subfolder with a `SKILL.md`. Earlier folders
/// win when names repeat. Returns problems found along the way.
pub fn discover(folders: &[(PathBuf, String)]) -> (Vec<Skill>, Vec<String>) {
    let mut skills: Vec<Skill> = Vec::new();
    let mut problems = Vec::new();
    for (folder, origin) in folders {
        let Ok(entries) = std::fs::read_dir(folder) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.join("SKILL.md").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            let file = dir.join("SKILL.md");
            let text = match std::fs::read_to_string(&file) {
                Ok(text) => text,
                Err(error) => {
                    problems.push(format!("{}: {error}", file.display()));
                    continue;
                }
            };
            let (fields, _) = front_matter(&text);
            let name = fields.get("name").cloned().unwrap_or_else(|| {
                dir.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let Some(description) = fields.get("description").filter(|d| !d.is_empty()) else {
                problems.push(format!(
                    "{}: no description in front matter",
                    file.display()
                ));
                continue;
            };
            if skills.iter().any(|skill| skill.name == name) {
                continue;
            }
            let hint = fields
                .get("when_to_use")
                .filter(|hint| !hint.is_empty())
                .map(|hint| hint.chars().take(HINT_CHARS).collect());
            skills.push(Skill {
                name,
                description: description.chars().take(DESCRIPTION_CHARS).collect(),
                dir,
                origin: origin.clone(),
                hint,
                eager: false,
            });
        }
    }
    (skills, problems)
}

/// The index lines for the skills loaded on use: name, description, and trigger hint.
pub(crate) fn index_lines(skills: &[Skill]) -> String {
    let mut text = String::new();
    for skill in skills.iter().filter(|skill| !skill.eager) {
        text.push_str(&format!("- {}: {}", skill.name, skill.description));
        if let Some(hint) = &skill.hint {
            text.push_str(&format!(" (use when: {hint})"));
        }
        text.push('\n');
    }
    text
}

/// The eager skills' instructions, for the prompt; `None` when no skill is eager.
pub fn eager_instructions(skills: &[Skill]) -> Option<String> {
    let loaded: Vec<String> = skills
        .iter()
        .filter(|skill| skill.eager)
        .filter_map(|skill| match load(skill, None) {
            Ok(text) => Some(text),
            Err(error) => {
                tracing::warn!(skill = %skill.name, %error, "could not read an eager skill");
                None
            }
        })
        .collect();
    (!loaded.is_empty()).then(|| {
        format!(
            "These skills are always loaded; follow each when its task comes up.\n\n{}",
            loaded.join("\n\n")
        )
    })
}

/// A skill's instructions, or one of its resource files (a path relative to the skill folder).
pub fn load(skill: &Skill, file: Option<&str>) -> Result<String, String> {
    let Some(file) = file.filter(|file| !file.is_empty()) else {
        let text = std::fs::read_to_string(skill.dir.join("SKILL.md"))
            .map_err(|error| error.to_string())?;
        let (_, body) = front_matter(&text);
        let resources = resources(&skill.dir);
        let mut out = format!("# Skill: {}\n\n{body}", skill.name);
        if !resources.is_empty() {
            out.push_str(&format!(
                "\n\nResource files (load with load_skill and file): {}",
                resources.join(", ")
            ));
        }
        return Ok(out);
    };
    let relative = Path::new(file);
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("{file} must be a path inside the skill folder"));
    }
    std::fs::read_to_string(skill.dir.join(relative)).map_err(|error| format!("{file}: {error}"))
}

/// Files in the skill folder other than `SKILL.md`, relative, two levels deep.
fn resources(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, depth: usize, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let relative = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if path.is_dir() && depth > 0 {
                walk(&path, &format!("{relative}/"), depth - 1, found);
            } else if path.is_file() && relative != "SKILL.md" {
                found.push(relative);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, "", 1, &mut found);
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(root: &Path, dir: &str, front: &str) {
        let dir = root.join(dir);
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\n{front}\n---\nDo the thing.\n"),
        )
        .unwrap();
        std::fs::write(dir.join("scripts/fill.py"), "print('hi')").unwrap();
    }

    #[test]
    fn skills_are_found_listed_and_loaded() {
        let user = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        write_skill(user.path(), "pdf", "name: pdf\ndescription: Fill PDF forms");
        write_skill(project.path(), "pdf", "name: pdf\ndescription: Shadowed");
        write_skill(project.path(), "notes", "description: Take notes");
        write_skill(project.path(), "broken", "name: broken");
        let (skills, problems) = discover(&[
            (user.path().to_path_buf(), "user".into()),
            (project.path().to_path_buf(), "project".into()),
        ]);
        let names: Vec<_> = skills
            .iter()
            .map(|s| (s.name.as_str(), s.origin.as_str()))
            .collect();
        assert_eq!(names, [("pdf", "user"), ("notes", "project")]);
        assert_eq!(problems.len(), 1);
        assert!(index_lines(&skills).contains("- pdf: Fill PDF forms\n"));

        let loaded = load(&skills[0], None).unwrap();
        assert!(loaded.contains("Do the thing."), "{loaded}");
        assert!(loaded.contains("scripts/fill.py"), "{loaded}");
        assert_eq!(
            load(&skills[0], Some("scripts/fill.py")).unwrap(),
            "print('hi')"
        );
        assert!(load(&skills[0], Some("../notes/SKILL.md")).is_err());
    }
}
