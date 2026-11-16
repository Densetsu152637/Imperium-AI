//! Named subagents: Markdown files with YAML front matter, in the Claude Code format, that a
//! plugin, the user or a project ships. The root agent delegates to one by name
//! (`spawn_agent`'s `agent` argument); the file's body is the subagent's instructions.
//!
//! ```text
//! ---
//! name: code-reviewer
//! description: Reviews a diff for bugs. Use after a change is written.
//! tools: read_file, grep        # or a YAML list; left out: the parent's tools
//! model: inherit                # or a model id or spec; left out: normal routing
//! ---
//! You review code. ...
//! ```
//!
//! Only each agent's name and description go in the root's prompt; the file's body is read into
//! the subagent when it starts. Sources, lowest precedence first, by name: plugins' `agents/`,
//! the user's `<config>/agents/`, the project's `.claude/agents/`, the project's
//! `.imperium/agents/`. A project's files are repository content, so they are read only for a
//! trusted project (the caller decides). A file can narrow what its agent may use and choose its
//! model; it never grants a permission: that is decided where tools run, not here.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Longest agent file read; a bigger one is repository noise, not instructions.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// Longest description kept.
const DESCRIPTION_CHARS: usize = 1_000;
/// Longest description line in the root's index.
const INDEX_CHARS: usize = 200;
/// Most agents listed in the root's index.
const INDEX_LIMIT: usize = 40;
/// How deep below an `agents/` folder files are looked for.
const MAX_DEPTH: usize = 3;

/// One named subagent, as its file defines it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    /// Kebab-case; what `spawn_agent`'s `agent` argument names.
    pub name: String,
    /// What the agent is for and when to use it.
    pub description: String,
    /// The tools the file lists, as written (Claude Code's names are mapped by [`resolve_tool`]);
    /// `None`: not narrowed, so the parent's tools.
    pub tools: Option<Vec<String>>,
    /// A model id or spec, or `inherit` (the delegating agent's model); `None`: the delegation's
    /// ordinary routing.
    pub model: Option<String>,
    /// The agent's instructions: the file's body.
    pub prompt: String,
    /// Where it came from: `plugin:<name>`, `user`, or `project`.
    pub origin: String,
    /// The file.
    pub path: PathBuf,
}

/// An `agents/` folder to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFolder {
    pub dir: PathBuf,
    pub origin: String,
}

impl AgentFolder {
    pub fn new(dir: PathBuf, origin: impl Into<String>) -> Self {
        AgentFolder {
            dir,
            origin: origin.into(),
        }
    }
}

/// A front matter value: text, or a list of text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Text(String),
    List(Vec<String>),
}

/// The fields of a front matter, in file order.
type Fields = Vec<(String, Value)>;

/// Reads agent files from `folders`, lowest precedence first: an agent of the same name in a
/// later folder replaces an earlier one. Returns the agents by name, and what to tell the user
/// (a file that could not be used, and the keys it has that are ignored).
pub fn discover(folders: &[AgentFolder]) -> (Vec<AgentDef>, Vec<String>) {
    let mut agents: BTreeMap<String, AgentDef> = BTreeMap::new();
    let mut notices = Vec::new();
    for folder in folders {
        let mut files = Vec::new();
        collect_files(&folder.dir, 0, &mut files);
        files.sort();
        // Names seen in this folder, so two files of one source do not silently shadow.
        let mut here: BTreeMap<String, PathBuf> = BTreeMap::new();
        for file in files {
            let (agent, mut found) = read_file(&file, &folder.origin);
            notices.append(&mut found);
            let Some(agent) = agent else {
                continue;
            };
            if let Some(first) = here.get(&agent.name) {
                notices.push(format!(
                    "{}: the agent {} is already defined by {}, so this file was ignored",
                    file.display(),
                    agent.name,
                    first.display()
                ));
                continue;
            }
            here.insert(agent.name.clone(), file);
            agents.insert(agent.name.clone(), agent);
        }
    }
    (agents.into_values().collect(), notices)
}

/// How many Markdown files a folder holds (what an untrusted project would have offered).
pub fn count_files(dir: &Path) -> usize {
    let mut files = Vec::new();
    collect_files(dir, 0, &mut files);
    files.len()
}

fn collect_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                collect_files(&path, depth + 1, out);
            }
        } else if path.extension().is_some_and(|extension| extension == "md") {
            out.push(path);
        }
    }
}

fn read_file(path: &Path, origin: &str) -> (Option<AgentDef>, Vec<String>) {
    let shown = path.display();
    // A symlink in a repository can point anywhere on the machine; its target is not
    // instructions someone chose to ship.
    if origin == "project"
        && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
    {
        return (
            None,
            vec![format!("{shown}: a symbolic link, so it was skipped")],
        );
    }
    // Regular files only (opening a FIFO would wait for a writer), and never more than the
    // limit is read, whatever the file grows to.
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return (
                None,
                vec![format!("{shown}: not a regular file, so it was skipped")],
            );
        }
        Ok(meta) if meta.len() > MAX_FILE_BYTES => return (None, too_big(path)),
        Err(error) => return (None, vec![format!("{shown}: {error}")]),
        Ok(_) => {}
    }
    let mut text = String::new();
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text));
    if let Err(error) = read {
        return (None, vec![format!("{shown}: {error}")]);
    }
    if text.len() as u64 > MAX_FILE_BYTES {
        return (None, too_big(path));
    }
    match parse(&text, origin, path) {
        Ok((agent, ignored)) => {
            let mut notices = Vec::new();
            if !ignored.is_empty() {
                notices.push(format!(
                    "{shown}: ignored the unsupported front matter key(s) {}",
                    ignored.join(", ")
                ));
            }
            (Some(agent), notices)
        }
        Err(error) => (None, vec![format!("{shown}: {error}, so it was skipped")]),
    }
}

fn too_big(path: &Path) -> Vec<String> {
    vec![format!(
        "{}: over {} KiB, so it was skipped",
        path.display(),
        MAX_FILE_BYTES / 1024
    )]
}

/// Whether `dir`, or a folder between it and `project`, is a symbolic link: a repository can
/// point such a folder anywhere on the machine.
pub fn is_linked_below(dir: &Path, project: &Path) -> bool {
    dir.ancestors()
        .take_while(|folder| *folder != project)
        .any(|folder| {
            std::fs::symlink_metadata(folder).is_ok_and(|meta| meta.file_type().is_symlink())
        })
}

/// Parses one agent file. Returns the agent and the front matter keys that were ignored.
pub fn parse(text: &str, origin: &str, path: &Path) -> Result<(AgentDef, Vec<String>), String> {
    let (fields, body) = front_matter(text)?;
    let mut ignored = Vec::new();
    let mut name = None;
    let mut description = None;
    let mut tools = None;
    let mut model = None;
    for (key, value) in fields {
        match key.as_str() {
            "name" => name = Some(scalar(&key, value)?),
            "description" => description = Some(scalar(&key, value)?),
            "model" => model = Some(scalar(&key, value)?),
            "tools" => {
                let items: Vec<String> = match value {
                    Value::Text(text) => text.split(',').map(str::to_string).collect(),
                    Value::List(items) => items,
                };
                let mut listed: Vec<String> = Vec::new();
                for item in items {
                    let item = item.trim().to_string();
                    if !item.is_empty() && !listed.contains(&item) {
                        listed.push(item);
                    }
                }
                tools = Some(listed);
            }
            _ => ignored.push(key.clone()),
        }
    }
    let name = name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .ok_or("the front matter has no name")?;
    if !is_kebab_case(&name) {
        return Err(format!(
            "the name {name:?} is not kebab-case (lowercase letters, digits and single hyphens)"
        ));
    }
    let description = description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty())
        .ok_or("the front matter has no description")?;
    let prompt = body.trim().to_string();
    if prompt.is_empty() {
        return Err("it has no instructions (nothing after the front matter)".into());
    }
    let model = model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    let agent = AgentDef {
        name,
        description: description.chars().take(DESCRIPTION_CHARS).collect(),
        // An empty list says nothing: the same as leaving `tools` out.
        tools: tools.filter(|tools| !tools.is_empty()),
        model,
        prompt,
        origin: origin.to_string(),
        path: path.to_path_buf(),
    };
    Ok((agent, ignored))
}

fn scalar(key: &str, value: Value) -> Result<String, String> {
    match value {
        Value::Text(text) => Ok(text),
        Value::List(_) => Err(format!("{key} must be text, not a list")),
    }
}

fn is_kebab_case(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The names of Imperium's tools that stand for one of Claude Code's tool names, so a file
/// written for Claude Code keeps working. Empty for any other name.
pub fn claude_tool_names(name: &str) -> &'static [&'static str] {
    match name {
        "Bash" => &["run_command", "command_output", "command_stop"],
        "Read" => &["read_file"],
        "Write" => &["write_file"],
        "Edit" | "MultiEdit" => &["edit_file"],
        "LS" => &["list_dir"],
        "Glob" => &["glob", "list_dir"],
        "Grep" => &["grep"],
        "WebFetch" => &["web_fetch"],
        "WebSearch" => &["web_search"],
        "Task" | "Agent" => &["spawn_agent"],
        _ => &[],
    }
}

/// What a tool name in an agent file stands for among `offered` (the tools the parent has):
/// the same name, or what Claude Code's name of that tool maps to. Empty when none of them is
/// offered.
pub fn resolve_tool<'a>(name: &str, offered: &'a [String]) -> Vec<&'a str> {
    if let Some(exact) = offered.iter().find(|offered| offered.as_str() == name) {
        return vec![exact.as_str()];
    }
    claude_tool_names(name)
        .iter()
        .filter_map(|mapped| {
            offered
                .iter()
                .find(|offered| offered.as_str() == *mapped)
                .map(String::as_str)
        })
        .collect()
}

/// The lines of the root's index: name, description (the first line, shortened) and source.
pub(crate) fn index_lines(agents: &[AgentDef]) -> String {
    let mut text = String::new();
    for agent in agents.iter().take(INDEX_LIMIT) {
        let first = agent
            .description
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        let mut shown: String = first.chars().take(INDEX_CHARS).collect();
        if first.chars().count() > INDEX_CHARS {
            shown.push_str("...");
        }
        text.push_str(&format!("- {}: {shown}\n", agent.name));
    }
    if agents.len() > INDEX_LIMIT {
        text.push_str(&format!("- ... and {} more\n", agents.len() - INDEX_LIMIT));
    }
    text
}

// ---- Front matter: the YAML subset agent files use ----

/// Splits `text` into its front matter fields (in file order) and its body.
fn front_matter(text: &str) -> Result<(Fields, &str), String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return Err("it has no front matter (the file must start with a --- line)".into());
    };
    let mut offset = 0;
    let mut closed = None;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            closed = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((yaml_end, body_start)) = closed else {
        return Err("the front matter is not closed by a --- line".into());
    };
    let fields = parse_yaml(&rest[..yaml_end])?;
    Ok((fields, &rest[body_start..]))
}

fn parse_yaml(yaml: &str) -> Result<Fields, String> {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut fields: Fields = Vec::new();
    // Where each key is in `fields`, so a repeated key replaces its value in one step.
    let mut positions: BTreeMap<String, usize> = BTreeMap::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let number = index + 1;
        index += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with([' ', '\t']) {
            return Err(format!(
                "front matter line {number}: unexpected indentation"
            ));
        }
        let Some((key, rest)) = line.split_once(':') else {
            return Err(format!("front matter line {number}: expected `key: value`"));
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!("front matter line {number}: {key:?} is not a key"));
        }
        let rest = rest.trim();
        let value = if rest.is_empty() {
            block_value(&lines, &mut index)
        } else if rest.starts_with(['|', '>']) {
            Value::Text(block_scalar(&lines, &mut index, rest.starts_with('>')))
        } else if rest.starts_with('[') {
            Value::List(inline_list(rest).map_err(|e| format!("front matter line {number}: {e}"))?)
        } else if rest.starts_with(['"', '\'']) {
            Value::Text(
                quoted(rest, &lines, &mut index)
                    .map_err(|e| format!("front matter line {number}: {e}"))?,
            )
        } else {
            Value::Text(plain(rest, &lines, &mut index))
        };
        match positions.get(key) {
            Some(&at) => fields[at].1 = value,
            None => {
                positions.insert(key.to_string(), fields.len());
                fields.push((key.to_string(), value));
            }
        }
    }
    Ok(fields)
}

fn is_indented(line: &str) -> bool {
    line.starts_with([' ', '\t'])
}

/// The value after `key:` with nothing on the line: a list (indented or not), or an indented
/// plain text, or empty.
fn block_value(lines: &[&str], index: &mut usize) -> Value {
    let mut items = Vec::new();
    let mut cursor = *index;
    while cursor < lines.len() {
        let line = lines[cursor];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            cursor += 1;
            continue;
        }
        let Some(item) = trimmed.strip_prefix('-') else {
            break;
        };
        if !item.is_empty() && !item.starts_with([' ', '\t']) {
            break;
        }
        items.push(unquote(item.trim()));
        cursor += 1;
    }
    if !items.is_empty() {
        *index = cursor;
        return Value::List(items);
    }
    let mut parts = Vec::new();
    while *index < lines.len() && is_indented(lines[*index]) {
        let part = lines[*index].trim();
        if !part.is_empty() {
            parts.push(part);
        }
        *index += 1;
    }
    Value::Text(parts.join(" "))
}

/// A `|` (literal) or `>` (folded) block: the indented lines after the key.
fn block_scalar(lines: &[&str], index: &mut usize, folded: bool) -> String {
    let mut block: Vec<&str> = Vec::new();
    while *index < lines.len() && (is_indented(lines[*index]) || lines[*index].trim().is_empty()) {
        block.push(lines[*index]);
        *index += 1;
    }
    let indent = block
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let dedented: Vec<&str> = block
        .iter()
        .map(|line| line.get(indent..).unwrap_or("").trim_end())
        .collect();
    let text = if folded {
        let mut out = String::new();
        let mut break_pending = false;
        for line in &dedented {
            if line.is_empty() {
                break_pending = true;
                continue;
            }
            if !out.is_empty() {
                out.push(if break_pending { '\n' } else { ' ' });
            }
            break_pending = false;
            out.push_str(line);
        }
        out
    } else {
        dedented.join("\n")
    };
    text.trim_end().to_string()
}

/// `[a, b, "c"]` on one line.
fn inline_list(text: &str) -> Result<Vec<String>, String> {
    let inner = text
        .strip_prefix('[')
        .and_then(|rest| rest.trim_end().strip_suffix(']'))
        .ok_or("a list opened with [ is not closed by ] on the same line")?;
    Ok(inner
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect())
}

/// A quoted value, which may continue on the following indented lines. Each line is scanned
/// once.
fn quoted(first: &str, lines: &[&str], index: &mut usize) -> Result<String, String> {
    let quote = first.chars().next().unwrap_or('"');
    let mut text = first.to_string();
    let mut value = String::new();
    let mut from = quote.len_utf8();
    loop {
        if let Some(end) = scan_quote(&text, from, quote, &mut value) {
            let after = text[end..].trim();
            if !after.is_empty() && !after.starts_with('#') {
                return Err(format!("text after the closing {quote}"));
            }
            return Ok(value);
        }
        from = text.len();
        if *index < lines.len() && (is_indented(lines[*index]) || lines[*index].trim().is_empty()) {
            text.push(' ');
            text.push_str(lines[*index].trim());
            *index += 1;
        } else {
            return Err(format!("a value opened with {quote} is never closed"));
        }
    }
}

/// Reads the value of a quoted text from byte `from` of `text` into `value`: the index just past
/// the closing quote, or `None` when the text ends first (the caller continues from its end).
fn scan_quote(text: &str, from: usize, quote: char, value: &mut String) -> Option<usize> {
    let mut chars = text[from..].char_indices();
    while let Some((offset, c)) = chars.next() {
        let position = from + offset;
        if quote == '"' && c == '\\' {
            let Some((_, escaped)) = chars.next() else {
                break;
            };
            value.push(match escaped {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '0' => '\0',
                other => other,
            });
        } else if c == quote {
            if quote == '\'' && text[position + 1..].starts_with('\'') {
                value.push('\'');
                chars.next();
            } else {
                return Some(position + 1);
            }
        } else {
            value.push(c);
        }
    }
    None
}

/// A plain value, which may continue on the following indented lines.
fn plain(first: &str, lines: &[&str], index: &mut usize) -> String {
    let mut text = first.to_string();
    while *index < lines.len() && is_indented(lines[*index]) && !lines[*index].trim().is_empty() {
        text.push(' ');
        text.push_str(lines[*index].trim());
        *index += 1;
    }
    text
}

/// `"x"` or `'x'` without its quotes; anything else as it is.
fn unquote(text: &str) -> String {
    for quote in ['"', '\''] {
        if text.len() >= 2 && text.starts_with(quote) && text.ends_with(quote) {
            let mut value = String::new();
            if scan_quote(text, quote.len_utf8(), quote, &mut value) == Some(text.len()) {
                return value;
            }
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(text: &str) -> AgentDef {
        parse(text, "user", Path::new("a.md")).unwrap().0
    }

    fn list(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|item| item.to_string()).collect())
    }

    fn parse_err(text: &str) -> String {
        parse(text, "user", Path::new("a.md")).unwrap_err()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn file(name: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n")
    }

    #[test]
    fn a_minimal_file_is_an_agent_that_inherits_everything() {
        let agent = parse_ok("---\nname: reviewer\ndescription: Reviews diffs\n---\nBe strict.\n");
        assert_eq!(agent.name, "reviewer");
        assert_eq!(agent.description, "Reviews diffs");
        assert_eq!(agent.prompt, "Be strict.");
        assert_eq!(agent.tools, None);
        assert_eq!(agent.model, None);
        assert_eq!(agent.origin, "user");
    }

    #[test]
    fn tools_are_a_comma_list_a_block_list_or_an_inline_list() {
        let comma = parse_ok("---\nname: a\ndescription: d\ntools: Read, Grep ,Bash\n---\nx");
        assert_eq!(comma.tools, list(&["Read", "Grep", "Bash"]));
        let block =
            parse_ok("---\nname: a\ndescription: d\ntools:\n  - Read\n  - \"Grep\"\n---\nx");
        assert_eq!(block.tools, list(&["Read", "Grep"]));
        let flush =
            parse_ok("---\nname: a\ndescription: d\ntools:\n- Read\n- grep\nmodel: m\n---\nx");
        assert_eq!(flush.tools, list(&["Read", "grep"]));
        assert_eq!(flush.model.as_deref(), Some("m"));
        let inline = parse_ok("---\nname: a\ndescription: d\ntools: [Read, 'Grep']\n---\nx");
        assert_eq!(inline.tools, list(&["Read", "Grep"]));
        // Nothing listed says nothing.
        for empty in ["tools:", "tools: ", "tools: []", "tools: \" \""] {
            let agent = parse_ok(&format!("---\nname: a\ndescription: d\n{empty}\n---\nx"));
            assert_eq!(agent.tools, None, "{empty}");
        }
    }

    #[test]
    fn descriptions_may_be_quoted_folded_or_blocks() {
        let quoted = parse_ok(
            "---\nname: a\ndescription: \"Use it when: x.\\n\\n<example>hi \\\"there\\\"</example>\"\n---\nx",
        );
        assert_eq!(
            quoted.description,
            "Use it when: x.\n\n<example>hi \"there\"</example>"
        );
        let single = parse_ok("---\nname: a\ndescription: 'it''s fine'\n---\nx");
        assert_eq!(single.description, "it's fine");
        let folded = parse_ok("---\nname: a\ndescription: >\n  one\n  two\n\n  three\n---\nx");
        assert_eq!(folded.description, "one two\nthree");
        let literal = parse_ok("---\nname: a\ndescription: |\n  one\n  two\n---\nx");
        assert_eq!(literal.description, "one\ntwo");
        let continued = parse_ok("---\nname: a\ndescription: starts here\n  and goes on\n---\nx");
        assert_eq!(continued.description, "starts here and goes on");
        let multi = parse_ok("---\nname: a\ndescription: \"first\n  second\"\n---\nx");
        assert_eq!(multi.description, "first second");
    }

    #[test]
    fn crlf_files_and_a_byte_order_mark_are_read() {
        let agent = parse_ok(
            "\u{feff}---\r\nname: a\r\ndescription: d\r\nmodel: inherit\r\n---\r\nBody\r\n",
        );
        assert_eq!(agent.name, "a");
        assert_eq!(agent.model.as_deref(), Some("inherit"));
        assert_eq!(agent.prompt, "Body");
    }

    #[test]
    fn unknown_keys_are_ignored_and_reported() {
        let (agent, ignored) = parse(
            "---\nname: a\ndescription: d\ncolor: blue\nskills:\n  - x\n---\nBody",
            "user",
            Path::new("a.md"),
        )
        .unwrap();
        assert_eq!(agent.name, "a");
        assert_eq!(ignored, vec!["color".to_string(), "skills".to_string()]);
    }

    #[test]
    fn a_file_that_cannot_be_an_agent_says_why() {
        assert!(parse_err("no front matter at all").contains("no front matter"));
        assert!(parse_err("---\nname: a\ndescription: d\nBody").contains("not closed"));
        assert!(parse_err("---\ndescription: d\n---\nBody").contains("no name"));
        assert!(parse_err("---\nname: a\n---\nBody").contains("no description"));
        assert!(parse_err("---\nname: a\ndescription: \"\"\n---\nBody").contains("no description"));
        assert!(
            parse_err("---\nname: Not Kebab\ndescription: d\n---\nBody").contains("kebab-case")
        );
        assert!(parse_err("---\nname: a--b\ndescription: d\n---\nBody").contains("kebab-case"));
        assert!(parse_err("---\nname: a\ndescription: d\n---\n\n  \n").contains("no instructions"));
        // Bad YAML: an unterminated quote, an unclosed list, a line that is not a key, stray
        // indentation, text after a closing quote, a list where text belongs.
        assert!(parse_err("---\nname: a\ndescription: \"open\n---\nBody").contains("never closed"));
        assert!(
            parse_err("---\nname: a\ndescription: d\ntools: [Read\n---\nBody")
                .contains("not closed")
        );
        assert!(
            parse_err("---\nname: a\ndescription: d\njust some words\n---\nBody")
                .contains("line 3")
        );
        assert!(parse_err("---\n  name: a\ndescription: d\n---\nBody").contains("indentation"));
        assert!(
            parse_err("---\nname: a\ndescription: \"x\" y\n---\nBody")
                .contains("after the closing")
        );
        assert!(parse_err("---\nname: a\ndescription:\n  - x\n---\nBody").contains("not a list"));
    }

    #[test]
    fn claude_code_tool_names_map_to_imperiums() {
        let offered: Vec<String> = ["read_file", "grep", "run_command", "web_fetch"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(resolve_tool("Read", &offered), vec!["read_file"]);
        assert_eq!(resolve_tool("grep", &offered), vec!["grep"]);
        assert_eq!(resolve_tool("Bash", &offered), vec!["run_command"]);
        assert_eq!(resolve_tool("WebFetch", &offered), vec!["web_fetch"]);
        // Not offered, or not a tool at all.
        assert!(resolve_tool("Write", &offered).is_empty());
        assert!(resolve_tool("telepathy", &offered).is_empty());
    }

    #[test]
    fn a_later_source_replaces_an_earlier_one_by_name() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugin");
        let user = root.path().join("user");
        let claude = root.path().join("proj/.claude/agents");
        let imperium = root.path().join("proj/.imperium/agents");
        write(
            &plugin.join("a.md"),
            &file("shared", "from plugin", "plugin body"),
        );
        write(
            &plugin.join("only-plugin.md"),
            &file("only-plugin", "p", "b"),
        );
        write(
            &user.join("a.md"),
            &file("shared", "from user", "user body"),
        );
        write(
            &claude.join("a.md"),
            &file("shared", "from .claude", "claude body"),
        );
        write(
            &claude.join("b.md"),
            &file("both", "from .claude", "claude body"),
        );
        write(
            &imperium.join("b.md"),
            &file("both", "from .imperium", "imperium body"),
        );
        let (agents, notices) = discover(&[
            AgentFolder::new(plugin, "plugin:p"),
            AgentFolder::new(user, "user"),
            AgentFolder::new(claude, "project"),
            AgentFolder::new(imperium, "project"),
        ]);
        assert!(notices.is_empty(), "{notices:?}");
        let by_name = |name: &str| agents.iter().find(|agent| agent.name == name).unwrap();
        assert_eq!(by_name("shared").description, "from .claude");
        assert_eq!(by_name("shared").origin, "project");
        assert_eq!(by_name("both").description, "from .imperium");
        assert_eq!(by_name("only-plugin").origin, "plugin:p");
        assert_eq!(agents.len(), 3);
        // By name, in order.
        assert!(agents.windows(2).all(|pair| pair[0].name < pair[1].name));
    }

    #[test]
    fn a_bad_file_is_reported_and_does_not_hide_the_others() {
        let root = tempfile::tempdir().unwrap();
        write(&root.path().join("good.md"), &file("good", "d", "b"));
        write(&root.path().join("bad.md"), "---\nname: bad\n---\nb");
        write(&root.path().join("twin.md"), &file("good", "d2", "b2"));
        write(&root.path().join("notes.txt"), "not markdown");
        write(
            &root.path().join("sub/deep.md"),
            "---\nname: deep\ndescription: d\ncolor: red\n---\nb",
        );
        let (agents, notices) = discover(&[AgentFolder::new(root.path().to_path_buf(), "user")]);
        let names: Vec<&str> = agents.iter().map(|agent| agent.name.as_str()).collect();
        assert_eq!(names, ["deep", "good"]);
        assert_eq!(notices.len(), 3, "{notices:?}");
        assert!(
            notices
                .iter()
                .any(|n| n.contains("bad.md") && n.contains("no description"))
        );
        assert!(notices.iter().any(|n| n.contains("already defined")));
        assert!(
            notices
                .iter()
                .any(|n| n.contains("deep.md") && n.contains("color"))
        );
        // The first of two files in one folder (by path) wins.
        assert_eq!(agents[1].description, "d");
    }

    #[test]
    fn a_large_many_line_file_is_read_in_one_pass() {
        // A quoted description of thousands of lines, and as many repeated keys.
        let mut text = String::from("---\nname: big\ndescription: \"first");
        for line in 0..6000 {
            text.push_str(&format!("\n  line {line} of the description"));
        }
        text.push_str("\"\n");
        for _ in 0..6000 {
            text.push_str("color: red\n");
        }
        text.push_str("---\nBody\n");
        let (agent, ignored) = parse(&text, "user", Path::new("big.md")).unwrap();
        assert!(agent.description.starts_with("first line 0 of"));
        assert!(agent.description.ends_with("line 5999 of the description"));
        assert_eq!(ignored, vec!["color".to_string()]);
    }

    #[test]
    fn project_folders_that_are_links_are_found() {
        let root = tempfile::tempdir().unwrap();
        assert!(!is_linked_below(
            &root.path().join(".claude/agents"),
            root.path()
        ));
        #[cfg(unix)]
        {
            let elsewhere = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(elsewhere.path(), root.path().join(".claude")).unwrap();
            assert!(is_linked_below(
                &root.path().join(".claude/agents"),
                root.path()
            ));
        }
    }

    #[test]
    fn an_oversized_file_is_skipped() {
        let root = tempfile::tempdir().unwrap();
        let big = "x".repeat(70 * 1024);
        write(&root.path().join("big.md"), &file("big", "d", &big));
        let (agents, notices) = discover(&[AgentFolder::new(root.path().to_path_buf(), "user")]);
        assert!(agents.is_empty());
        assert!(notices[0].contains("KiB"), "{notices:?}");
    }

    #[test]
    fn the_index_lists_names_and_first_lines_only() {
        let mut agent = parse_ok(&file("reviewer", "Reviews diffs.", "SECRET BODY"));
        agent.description = format!("{}\nsecond line", "a".repeat(250));
        let text = index_lines(&[agent]);
        assert!(text.starts_with("- reviewer: aaa"), "{text}");
        assert!(text.contains("...") && !text.contains("second line"));
        assert!(!text.contains("SECRET"));
        assert_eq!(text.lines().count(), 1);
    }
}
