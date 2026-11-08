//! Agent Skills: folders with a `SKILL.md` (name and description in front matter, instructions
//! in the body) and optional resource files. Agents see each skill's description and load the
//! ones they need with the `load_skill` tool.

use std::path::{Component, Path, PathBuf};

use imperium_api::markdown::front_matter;

/// Longest description shown in the skill list.
const DESCRIPTION_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The folder holding `SKILL.md` and its resources.
    pub dir: PathBuf,
    /// Where it came from, such as `user`, `project`, or `plugin:<name>`.
    pub origin: String,
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
            skills.push(Skill {
                name,
                description: description.chars().take(DESCRIPTION_CHARS).collect(),
                dir,
                origin: origin.clone(),
            });
        }
    }
    (skills, problems)
}

/// The skill list for an agent's instructions.
pub fn listing(skills: &[Skill]) -> String {
    let mut text = String::from(
        "Skills are packaged instructions for particular tasks. When a task matches a skill's \
         description, call load_skill with its name before starting, and follow it.\n",
    );
    for skill in skills {
        text.push_str(&format!("- {}: {}\n", skill.name, skill.description));
    }
    text
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
        assert!(listing(&skills).contains("- pdf: Fill PDF forms"));

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
