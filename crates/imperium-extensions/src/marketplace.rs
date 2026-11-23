//! Claude Code plugin marketplaces: parses `.claude-plugin/marketplace.json`.
//!
//! ```json
//! { "name": "acme", "owner": {"name": "Acme"}, "metadata": {"pluginRoot": "./plugins"},
//!   "plugins": [
//!     {"name": "fmt", "source": "fmt", "description": "Formats"},
//!     {"name": "lint", "source": {"source": "github", "repo": "acme/lint", "ref": "v1"}},
//!     {"name": "docs", "source": {"source": "url", "url": "https://example.test/docs.git"}} ] }
//! ```
//!
//! Parsing is pure: [`parse`] takes text, [`read`] reads the one file in a given folder, and
//! nothing here fetches, runs or installs anything. Unknown fields are ignored. A marketplace
//! that is not JSON, has no valid name or has no `plugins` list is an error; a bad entry (a name
//! that cannot be a folder name, a source that leaves the repository, a duplicate name) is left
//! out and reported in [`Marketplace::problems`], so one bad entry never hides the rest.
//!
//! Choices the format leaves open:
//!
//! - A relative `source` that starts with `./` is relative to the repository root; a bare one
//!   (`"fmt"`) is relative to `metadata.pluginRoot` when it is set (and to the root when not).
//!   Either way the path is stored normalised (no `./`, `/` separators) and is refused when it
//!   is absolute, holds `..` or a drive or stream colon, because it must stay inside the
//!   repository. `"./"` is the repository root itself.
//! - Only `https://` Git URLs are accepted, without credentials in the URL; other schemes are
//!   an invalid entry rather than an unsupported one, since they would reach the fetch step.
//! - Any other `source` type (`npm`, `git-subdir`, ...) is [`Source::Unsupported`], listed but
//!   not installable.
//! - [`Entry::canonical_json`] writes the entry as it was given (unknown fields included), with
//!   sorted keys and no insignificant whitespace, so the same entry always gives the same bytes.

use std::path::Path;

use serde_json::Value;

use crate::plugins::{Author, Fields};
pub use crate::plugins::{ENTRY_FILE, canonical_json};

/// Where a marketplace's manifest lives inside its repository.
pub const MARKETPLACE_FILE: &str = ".claude-plugin/marketplace.json";

/// Most of a marketplace file that is read.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Marketplace {
    pub name: String,
    pub owner: Option<Author>,
    pub description: Option<String>,
    pub version: Option<String>,
    /// `metadata.pluginRoot`, normalised; `None` when unset or the repository root.
    pub plugin_root: Option<String>,
    pub plugins: Vec<Entry>,
    /// Entries that were left out, and why.
    pub problems: Vec<String>,
}

impl Marketplace {
    pub fn plugin(&self, name: &str) -> Option<&Entry> {
        self.plugins.iter().find(|entry| entry.name == name)
    }
}

/// One plugin a marketplace lists.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub source: Source,
    /// The manifest-like fields (description, version, author, ... and the component paths).
    pub fields: Fields,
    pub category: Option<String>,
    pub tags: Vec<String>,
    /// `false` means the entry is the plugin's manifest and the plugin needs no `plugin.json`.
    pub strict: bool,
    raw: Value,
}

impl Entry {
    /// The entry as canonical JSON: sorted keys, no insignificant whitespace.
    pub fn canonical_json(&self) -> String {
        canonical_json(&self.raw)
    }
}

/// Where a plugin's files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A folder inside the marketplace's own repository, normalised, relative to its root.
    Path(String),
    Github {
        /// `owner/name`.
        repo: String,
        git_ref: Option<String>,
        sha: Option<String>,
    },
    Url {
        url: String,
        git_ref: Option<String>,
        sha: Option<String>,
    },
    /// A source type this version does not install (`npm`, `git-subdir`, ...).
    Unsupported(String),
}

impl Source {
    /// The Git URL to clone, for the sources that live in their own repository.
    pub fn clone_url(&self) -> Option<String> {
        match self {
            Source::Github { repo, .. } => Some(format!("https://github.com/{repo}.git")),
            Source::Url { url, .. } => Some(url.clone()),
            Source::Path(_) | Source::Unsupported(_) => None,
        }
    }

    /// The pinned branch or tag, when the entry names one.
    pub fn git_ref(&self) -> Option<&str> {
        match self {
            Source::Github { git_ref, .. } | Source::Url { git_ref, .. } => git_ref.as_deref(),
            _ => None,
        }
    }

    /// The pinned commit, when the entry names one.
    pub fn sha(&self) -> Option<&str> {
        match self {
            Source::Github { sha, .. } | Source::Url { sha, .. } => sha.as_deref(),
            _ => None,
        }
    }
}

/// A name that is safe as a folder name: letters, digits, `.`, `_`, `-`, not starting with a dot.
/// The same rule as installing an extension by name.
pub fn check_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        // Windows drops a trailing dot: `foo.` would name the folder `foo`.
        && !name.ends_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(name.to_string())
    } else {
        Err(format!(
            "the name {name:?} is not valid: use 1-64 letters, digits, '.', '_' or '-', not starting or ending with a dot"
        ))
    }
}

/// Reads and parses `<folder>/.claude-plugin/marketplace.json`. Neither the `.claude-plugin`
/// folder nor the file may be a link, the file must be a regular file, and at most 4 MiB of it
/// is read.
pub fn read(folder: &Path) -> Result<Marketplace, String> {
    use std::io::Read;

    let fail = |error: &dyn std::fmt::Display| format!("{MARKETPLACE_FILE}: {error}");
    let dir = folder.join(".claude-plugin");
    let path = folder.join(MARKETPLACE_FILE);
    let dir_meta = std::fs::symlink_metadata(&dir).map_err(|error| fail(&error))?;
    if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
        return Err(".claude-plugin is not a plain folder".to_string());
    }
    let meta = std::fs::symlink_metadata(&path).map_err(|error| fail(&error))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("{MARKETPLACE_FILE} is not a regular file"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .map_err(|error| fail(&error))?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| fail(&error))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(format!("{MARKETPLACE_FILE} is larger than 4 MiB"));
    }
    let text = String::from_utf8(bytes).map_err(|error| fail(&error))?;
    parse(&text)
}

/// Parses marketplace JSON.
pub fn parse(text: &str) -> Result<Marketplace, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("marketplace.json: {error}"))?;
    let root = value
        .as_object()
        .ok_or("marketplace.json: expected a JSON object")?;
    let name = root
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "marketplace.json: it has no name".to_string())
        .and_then(|name| check_name(name).map_err(|error| format!("marketplace.json: {error}")))?;
    let metadata = root.get("metadata");
    let plugin_root = match metadata
        .and_then(|metadata| metadata.get("pluginRoot"))
        .and_then(Value::as_str)
    {
        Some(root) => {
            let normalised = normalise(root)
                .map_err(|error| format!("marketplace.json: metadata.pluginRoot {error}"))?;
            (!normalised.is_empty()).then_some(normalised)
        }
        None => None,
    };
    let listed = root
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or("marketplace.json: it has no plugins list")?;
    let mut plugins: Vec<Entry> = Vec::new();
    let mut problems = Vec::new();
    for (index, item) in listed.iter().enumerate() {
        let label = item
            .get("name")
            .and_then(Value::as_str)
            .map_or_else(|| format!("plugin #{}", index + 1), str::to_string);
        match entry(item, plugin_root.as_deref(), &mut problems) {
            Ok(entry) if plugins.iter().any(|seen| seen.name == entry.name) => {
                problems.push(format!("{label}: the name is listed more than once"));
            }
            Ok(entry) => plugins.push(entry),
            Err(error) => problems.push(format!("{label}: {error}")),
        }
    }
    let text_of = |key: &str| {
        metadata
            .and_then(|metadata| metadata.get(key))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    };
    Ok(Marketplace {
        name,
        owner: root.get("owner").and_then(Author::from_value),
        description: text_of("description"),
        version: text_of("version"),
        plugin_root,
        plugins,
        problems,
    })
}

fn entry(
    value: &Value,
    plugin_root: Option<&str>,
    notes: &mut Vec<String>,
) -> Result<Entry, String> {
    if !value.is_object() {
        return Err("expected a JSON object".into());
    }
    let mut fields = Fields::from_value(value, notes);
    let name = fields
        .name
        .take()
        .ok_or_else(|| "it has no name".to_string())
        .and_then(|name| check_name(&name))?;
    let source = match value.get("source") {
        None => return Err("it has no source".into()),
        Some(source) => parse_source(source, plugin_root)?,
    };
    let strings = |key: &str| -> Vec<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(Entry {
        name,
        source,
        fields,
        category: value
            .get("category")
            .and_then(Value::as_str)
            .filter(|category| !category.is_empty())
            .map(str::to_string),
        tags: strings("tags"),
        strict: value.get("strict").and_then(Value::as_bool).unwrap_or(true),
        raw: value.clone(),
    })
}

/// Parses an entry's `source`; `plugin_root` applies to bare relative paths.
fn parse_source(source: &Value, plugin_root: Option<&str>) -> Result<Source, String> {
    match source {
        Value::String(path) => relative_source(path, plugin_root),
        Value::Object(object) => {
            let kind = object.get("source").and_then(Value::as_str);
            let text = |key: &str| object.get(key).and_then(Value::as_str);
            match kind {
                Some("github") => {
                    let repo = text("repo").ok_or("the github source has no repo")?;
                    Ok(Source::Github {
                        repo: check_repo(repo)?,
                        git_ref: optional_ref(text("ref"))?,
                        sha: optional_sha(text("sha"))?,
                    })
                }
                Some("url") => {
                    let url = text("url").ok_or("the url source has no url")?;
                    Ok(Source::Url {
                        url: check_url(url)?,
                        git_ref: optional_ref(text("ref"))?,
                        sha: optional_sha(text("sha"))?,
                    })
                }
                Some(other) => Ok(Source::Unsupported(other.to_string())),
                None => Ok(Source::Unsupported("unknown".into())),
            }
        }
        _ => Ok(Source::Unsupported("unknown".into())),
    }
}

fn relative_source(path: &str, plugin_root: Option<&str>) -> Result<Source, String> {
    let from_root = path == "." || path.starts_with("./") || path.starts_with(".\\");
    let joined = match plugin_root {
        Some(base) if !from_root => format!("{base}/{path}"),
        _ => path.to_string(),
    };
    normalise(&joined)
        .map(Source::Path)
        .map_err(|error| format!("the source {path:?} {error}"))
}

/// A relative path as `a/b` without `.` parts; refused when it could leave the repository.
fn normalise(path: &str) -> Result<String, String> {
    let bytes = path.as_bytes();
    if path.starts_with('/') || path.starts_with('\\') || bytes.get(1) == Some(&b':') {
        return Err("must be relative to the repository".into());
    }
    let mut parts = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err("leaves the repository".into()),
            part if part.contains([':', '\0']) => {
                return Err("holds a character a path cannot".into());
            }
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

fn check_repo(repo: &str) -> Result<String, String> {
    let repo = repo.trim();
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && !part.starts_with(['-', '.'])
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    match repo.split_once('/') {
        Some((owner, name)) if valid_part(owner) && valid_part(name) => Ok(repo.to_string()),
        _ => Err(format!("the repo {repo:?} is not of the form owner/name")),
    }
}

fn check_url(url: &str) -> Result<String, String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("the url {url:?} must start with https://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!(
            "the url {url:?} must have a host, no credentials and no spaces"
        ));
    }
    Ok(url.to_string())
}

fn optional_ref(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let bad = value.is_empty()
        || value.len() > 200
        || value.starts_with(['-', '/'])
        || value.contains("..")
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c));
    if bad {
        Err(format!("the ref {value:?} is not a branch or tag name"))
    } else {
        Ok(Some(value.to_string()))
    }
}

fn optional_sha(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    if matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(value.to_ascii_lowercase()))
    } else {
        Err(format!("the sha {value:?} is not a full commit hash"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(source: &str) -> Result<Marketplace, String> {
        parse(&format!(
            r#"{{"name": "m", "plugins": [{{"name": "p", "source": {source}}}]}}"#
        ))
    }

    fn source_of(source: &str) -> Source {
        let market = one(source).unwrap();
        assert!(market.problems.is_empty(), "{:?}", market.problems);
        market.plugins[0].source.clone()
    }

    #[test]
    fn every_source_form_parses() {
        assert_eq!(
            source_of(r#""./plugins/p""#),
            Source::Path("plugins/p".into())
        );
        assert_eq!(source_of(r#""./""#), Source::Path(String::new()));
        let github = source_of(
            r#"{"source": "github", "repo": "acme/p", "ref": "v1", "sha": "0123456789abcdef0123456789abcdef01234567"}"#,
        );
        assert_eq!(
            github,
            Source::Github {
                repo: "acme/p".into(),
                git_ref: Some("v1".into()),
                sha: Some("0123456789abcdef0123456789abcdef01234567".into()),
            }
        );
        assert_eq!(
            github.clone_url().as_deref(),
            Some("https://github.com/acme/p.git")
        );
        assert_eq!(github.git_ref(), Some("v1"));
        let url = source_of(r#"{"source": "url", "url": "https://example.test/p.git"}"#);
        assert_eq!(
            url,
            Source::Url {
                url: "https://example.test/p.git".into(),
                git_ref: None,
                sha: None,
            }
        );
        assert_eq!(
            url.clone_url().as_deref(),
            Some("https://example.test/p.git")
        );
        assert_eq!(url.sha(), None);
    }

    #[test]
    fn other_source_types_are_listed_as_unsupported() {
        assert_eq!(
            source_of(r#"{"source": "npm", "package": "p"}"#),
            Source::Unsupported("npm".into())
        );
        assert_eq!(
            source_of(r#"{"source": "git-subdir", "url": "https://x.test/r.git"}"#),
            Source::Unsupported("git-subdir".into())
        );
        assert_eq!(source_of("42"), Source::Unsupported("unknown".into()));
        assert_eq!(
            source_of(r#"{"x": 1}"#),
            Source::Unsupported("unknown".into())
        );
        assert_eq!(source_of("42").clone_url(), None);
    }

    #[test]
    fn plugin_root_applies_to_bare_paths_only() {
        let market = parse(
            r#"{"name": "m", "metadata": {"pluginRoot": "./plugins"}, "plugins": [
                {"name": "bare", "source": "fmt"},
                {"name": "rooted", "source": "./tools/lint"}]}"#,
        )
        .unwrap();
        assert_eq!(market.plugin_root.as_deref(), Some("plugins"));
        assert_eq!(
            market.plugin("bare").unwrap().source,
            Source::Path("plugins/fmt".into())
        );
        assert_eq!(
            market.plugin("rooted").unwrap().source,
            Source::Path("tools/lint".into())
        );
    }

    #[test]
    fn a_source_that_leaves_the_repository_is_refused() {
        for bad in [
            r#""../outside""#,
            r#""./a/../../b""#,
            r#""/etc""#,
            r#""C:\\x""#,
            r#""..\\x""#,
        ] {
            let market = one(bad).unwrap();
            assert!(market.plugins.is_empty(), "{bad}");
            assert_eq!(market.problems.len(), 1, "{bad}");
        }
        let root_escape =
            parse(r#"{"name": "m", "metadata": {"pluginRoot": ".."}, "plugins": []}"#);
        assert!(root_escape.unwrap_err().contains("pluginRoot"));
    }

    #[test]
    fn bad_git_sources_are_refused_but_do_not_hide_other_entries() {
        let market = parse(
            r#"{"name": "m", "plugins": [
                {"name": "a", "source": {"source": "url", "url": "file:///etc"}},
                {"name": "b", "source": {"source": "url", "url": "https://u:p@x.test/r.git"}},
                {"name": "c", "source": {"source": "github", "repo": "-x/y"}},
                {"name": "d", "source": {"source": "github", "repo": "x/y", "ref": "--upload-pack=z"}},
                {"name": "e", "source": {"source": "github", "repo": "x/y", "sha": "abc"}},
                {"name": "ok", "source": {"source": "github", "repo": "x/y"}}]}"#,
        )
        .unwrap();
        assert_eq!(market.plugins.len(), 1);
        assert_eq!(market.problems.len(), 5);
    }

    #[test]
    fn names_are_checked_and_duplicates_dropped() {
        let market = parse(
            r#"{"name": "m", "plugins": [
                {"name": "../evil", "source": "./a"},
                {"name": ".hidden", "source": "./a"},
                {"name": "dup", "source": "./a"},
                {"name": "dup", "source": "./b"},
                {"source": "./c"}]}"#,
        )
        .unwrap();
        assert_eq!(market.plugins.len(), 1);
        assert_eq!(market.plugins[0].source, Source::Path("a".into()));
        assert_eq!(market.problems.len(), 4);
        assert!(parse(r#"{"name": "bad name", "plugins": []}"#).is_err());
        assert!(parse(r#"{"plugins": []}"#).is_err());
        assert!(parse(r#"{"name": "m"}"#).is_err());
        assert!(parse("[]").is_err());
    }

    #[test]
    fn a_name_ending_in_a_dot_is_refused() {
        assert!(check_name("foo.").is_err() && check_name("foo ").is_ok_and(|name| name == "foo"));
        let market = parse(
            r#"{"name": "m", "plugins": [
                {"name": "foo.", "source": "./a"},
                {"name": "foo", "source": "./b"}]}"#,
        )
        .unwrap();
        assert_eq!(market.plugins.len(), 1);
        assert_eq!(market.plugins[0].name, "foo");
        assert_eq!(market.problems.len(), 1);
        assert!(parse(r#"{"name": "m.", "plugins": []}"#).is_err());
    }

    #[test]
    fn entry_fields_and_unknown_fields() {
        let market = parse(
            r#"{"name": "m", "owner": {"name": "Acme", "email": "a@x.test"},
                "metadata": {"description": "d", "version": "2"}, "future": [1, 2],
                "plugins": [{"name": "p", "source": "./p", "description": "does it",
                    "version": "1.2.3", "author": {"name": "Ann"}, "homepage": "https://h.test",
                    "repository": "https://r.test", "license": "MIT", "keywords": ["a", "b"],
                    "category": "tools", "tags": ["x"], "strict": false, "surprise": {"a": 1},
                    "commands": ["./c"], "mcpServers": {"s": {"command": "run"}}}]}"#,
        )
        .unwrap();
        assert_eq!(market.owner.as_ref().unwrap().name, "Acme");
        assert_eq!(market.description.as_deref(), Some("d"));
        assert_eq!(market.version.as_deref(), Some("2"));
        let entry = &market.plugins[0];
        assert_eq!(entry.fields.description, "does it");
        assert_eq!(entry.fields.version.as_deref(), Some("1.2.3"));
        assert_eq!(entry.fields.author.as_ref().unwrap().name, "Ann");
        assert_eq!(entry.fields.homepage.as_deref(), Some("https://h.test"));
        assert_eq!(entry.fields.license.as_deref(), Some("MIT"));
        assert_eq!(entry.fields.keywords, ["a", "b"]);
        assert_eq!(entry.category.as_deref(), Some("tools"));
        assert_eq!(entry.tags, ["x"]);
        assert!(!entry.strict);
        assert_eq!(entry.fields.components.commands, ["./c"]);
        assert_eq!(entry.fields.components.mcp_servers.len(), 1);
        // `strict` defaults to true.
        let plain = parse(r#"{"name": "m", "plugins": [{"name": "p", "source": "./p"}]}"#).unwrap();
        assert!(plain.plugins[0].strict);
    }

    #[test]
    fn canonical_json_is_stable_across_key_order() {
        let a = parse(
            r#"{"name": "m", "plugins": [{"name": "p", "source": {"source": "github", "repo": "a/b"},
                "description": "d", "keywords": ["z", "a"], "x": {"b": 1, "a": [{"d": 1, "c": 2}]}}]}"#,
        )
        .unwrap();
        let b = parse(
            r#"{"plugins": [{"x": {"a": [{"c": 2, "d": 1}], "b": 1}, "keywords": ["z", "a"],
                "description": "d", "source": {"repo": "a/b", "source": "github"}, "name": "p"}],
                "name": "m"}"#,
        )
        .unwrap();
        let json = a.plugins[0].canonical_json();
        assert_eq!(json, b.plugins[0].canonical_json());
        assert_eq!(
            json,
            r#"{"description":"d","keywords":["z","a"],"name":"p","source":{"repo":"a/b","source":"github"},"x":{"a":[{"c":2,"d":1}],"b":1}}"#
        );
    }

    #[test]
    fn read_loads_the_file_in_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_err());
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(MARKETPLACE_FILE),
            r#"{"name": "m", "plugins": []}"#,
        )
        .unwrap();
        assert_eq!(read(dir.path()).unwrap().name, "m");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_marketplace_file_or_folder_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join(".claude-plugin")).unwrap();
        let body = r#"{"name": "m", "plugins": []}"#;
        std::fs::write(outside.join(".claude-plugin/marketplace.json"), body).unwrap();

        let linked_file = dir.path().join("a");
        std::fs::create_dir_all(linked_file.join(".claude-plugin")).unwrap();
        std::os::unix::fs::symlink(
            outside.join(".claude-plugin/marketplace.json"),
            linked_file.join(MARKETPLACE_FILE),
        )
        .unwrap();
        assert!(read(&linked_file).unwrap_err().contains("regular file"));

        let linked_dir = dir.path().join("b");
        std::fs::create_dir_all(&linked_dir).unwrap();
        std::os::unix::fs::symlink(
            outside.join(".claude-plugin"),
            linked_dir.join(".claude-plugin"),
        )
        .unwrap();
        assert!(read(&linked_dir).is_err());
    }
}
