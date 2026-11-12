use std::time::Duration;

use imperium_api::ToolSpec;
use serde::Deserialize;
use serde_json::json;

use crate::exec::DEFAULT_TIMEOUT_S;
use crate::search::{GrepQuery, Scope};
use crate::{Workspace, exec, files, search};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool {0:?}")]
    UnknownTool(String),
    #[error("invalid arguments: {0}")]
    Invalid(String),
    #[error("{0} is outside the workspace")]
    OutsideWorkspace(String),
    #[error("{0} is read-only: a read-only grant or mount allows reading it, not writing")]
    ReadOnly(String),
    #[error("{0}")]
    Io(String),
    #[error("timed out after {0} s")]
    Timeout(u64),
}

/// What a tool does to the machine, which decides whether it needs the user's approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}

/// A parsed, validated tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    ReadFile {
        path: String,
        offset: Option<usize>,
        limit: Option<usize>,
    },
    WriteFile {
        path: String,
        content: String,
    },
    EditFile {
        path: String,
        old_string: String,
        new_string: String,
    },
    ListDir {
        path: String,
    },
    Glob {
        pattern: String,
        scope: Scope,
    },
    Grep {
        query: GrepQuery,
        scope: Scope,
    },
    RunCommand {
        command: String,
        timeout_s: u64,
        /// Start it and return its id at once (see `command_output`, `command_stop`).
        background: bool,
    },
    /// New output of a background command.
    CommandOutput {
        id: String,
        since: Option<u64>,
    },
    /// Stops a background command.
    CommandStop {
        id: String,
    },
}

#[derive(Deserialize)]
struct SearchArgs {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    context: Option<usize>,
    #[serde(default)]
    max_results: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    include_ignored: bool,
}

impl SearchArgs {
    fn scope(&self) -> Scope {
        Scope {
            path: self
                .path
                .as_deref()
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .unwrap_or(".")
                .to_string(),
            include_ignored: self.include_ignored,
            offset: self.offset.unwrap_or(0),
            max_results: self
                .max_results
                .unwrap_or(search::DEFAULT_RESULTS)
                .clamp(1, search::MAX_RESULTS),
        }
    }
}

#[derive(Deserialize)]
struct ProcessArgs {
    id: String,
    #[serde(default)]
    since: Option<u64>,
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
}

#[derive(Deserialize)]
struct CommandArgs {
    command: String,
    #[serde(default)]
    timeout_s: Option<u64>,
    #[serde(default)]
    background: bool,
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, ToolError> {
    let arguments = if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    };
    serde_json::from_str(arguments).or_else(|error| {
        // Models often leave a Windows path's backslashes unescaped (`"D:\Git Repositories"`),
        // which is not JSON; read such a backslash as itself.
        serde_json::from_str(&escape_backslashes(arguments, true))
            .map_err(|_| ToolError::Invalid(error.to_string()))
    })
}

/// Arguments that name only a path. A path never holds control characters, so one that does
/// came from unescaped backslashes that happen to form JSON escapes (`D:\new\tools` holds a
/// newline and a tab); read every backslash in it as itself instead.
fn parse_path_args(arguments: &str) -> Result<PathArgs, ToolError> {
    let args: PathArgs = parse(arguments)?;
    if !args.path.chars().any(char::is_control) {
        return Ok(args);
    }
    match serde_json::from_str::<PathArgs>(&escape_backslashes(arguments, false)) {
        Ok(literal) if !literal.path.chars().any(char::is_control) => Ok(literal),
        _ => Ok(args),
    }
}

/// `text` with each backslash that does not already start an escaped backslash or quote
/// doubled, so it reads as a literal backslash. With `keep_escapes`, valid JSON escapes
/// (`\n`, `\t`, `\u0041`, …) are kept too.
fn escape_backslashes(text: &str, keep_escapes: bool) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c != '\\' {
            out.push(c);
            index += 1;
            continue;
        }
        let next = chars.get(index + 1).copied();
        let unicode = next == Some('u')
            && chars
                .get(index + 2..index + 6)
                .is_some_and(|hex| hex.iter().all(char::is_ascii_hexdigit));
        let kept = matches!(next, Some('\\' | '"'))
            || (keep_escapes
                && (unicode || matches!(next, Some('/' | 'b' | 'f' | 'n' | 'r' | 't'))));
        if kept {
            out.push('\\');
            out.extend(next);
            index += 2;
        } else {
            out.push_str("\\\\");
            index += 1;
        }
    }
    out
}

impl Invocation {
    /// Parses a model's tool call (name and JSON arguments).
    pub fn parse(name: &str, arguments: &str) -> Result<Self, ToolError> {
        Ok(match name {
            "read_file" => {
                let args = parse_path_args(arguments)?;
                Invocation::ReadFile {
                    path: args.path,
                    offset: args.offset,
                    limit: args.limit,
                }
            }
            "write_file" => {
                let args: WriteArgs = parse(arguments)?;
                Invocation::WriteFile {
                    path: args.path,
                    content: args.content,
                }
            }
            "edit_file" => {
                let args: EditArgs = parse(arguments)?;
                Invocation::EditFile {
                    path: args.path,
                    old_string: args.old_string,
                    new_string: args.new_string,
                }
            }
            "list_dir" => {
                let args = parse_path_args(arguments)?;
                Invocation::ListDir { path: args.path }
            }
            "glob" => {
                let args: SearchArgs = parse(arguments)?;
                Invocation::Glob {
                    scope: args.scope(),
                    pattern: args.pattern,
                }
            }
            "grep" => {
                let args: SearchArgs = parse(arguments)?;
                Invocation::Grep {
                    scope: args.scope(),
                    query: GrepQuery {
                        pattern: args.pattern,
                        glob: args.glob.filter(|glob| !glob.trim().is_empty()),
                        case_insensitive: args.case_insensitive,
                        context: args.context.unwrap_or(0).min(search::MAX_CONTEXT),
                    },
                }
            }
            "run_command" => {
                let args: CommandArgs = parse(arguments)?;
                Invocation::RunCommand {
                    command: args.command,
                    timeout_s: args.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, 3600),
                    background: args.background,
                }
            }
            "command_output" => {
                let args: ProcessArgs = parse(arguments)?;
                Invocation::CommandOutput {
                    id: args.id,
                    since: args.since,
                }
            }
            "command_stop" => {
                let args: ProcessArgs = parse(arguments)?;
                Invocation::CommandStop { id: args.id }
            }
            other => return Err(ToolError::UnknownTool(other.to_string())),
        })
    }

    /// What the call does. Reading a background command's output and stopping it count as
    /// reads: they touch only what the session itself started.
    pub fn access(&self) -> Access {
        match self {
            Invocation::ReadFile { .. }
            | Invocation::ListDir { .. }
            | Invocation::Glob { .. }
            | Invocation::Grep { .. }
            | Invocation::CommandOutput { .. }
            | Invocation::CommandStop { .. } => Access::Read,
            Invocation::WriteFile { .. } | Invocation::EditFile { .. } => Access::Write,
            Invocation::RunCommand { .. } => Access::Execute,
        }
    }

    /// The path a file tool names, for rule patterns; `None` for commands and processes.
    pub fn path(&self) -> Option<&str> {
        match self {
            Invocation::ReadFile { path, .. }
            | Invocation::WriteFile { path, .. }
            | Invocation::EditFile { path, .. }
            | Invocation::ListDir { path } => Some(path),
            Invocation::Glob { scope, .. } | Invocation::Grep { scope, .. } => Some(&scope.path),
            Invocation::RunCommand { .. }
            | Invocation::CommandOutput { .. }
            | Invocation::CommandStop { .. } => None,
        }
    }

    pub fn tool_name(&self) -> &'static str {
        match self {
            Invocation::ReadFile { .. } => "read_file",
            Invocation::WriteFile { .. } => "write_file",
            Invocation::EditFile { .. } => "edit_file",
            Invocation::ListDir { .. } => "list_dir",
            Invocation::Glob { .. } => "glob",
            Invocation::Grep { .. } => "grep",
            Invocation::RunCommand { .. } => "run_command",
            Invocation::CommandOutput { .. } => "command_output",
            Invocation::CommandStop { .. } => "command_stop",
        }
    }

    /// One line describing the call, for permission prompts and logs.
    pub fn summary(&self) -> String {
        match self {
            Invocation::ReadFile { path, .. } => format!("read {path}"),
            Invocation::WriteFile { path, content } => {
                format!("write {path} ({} bytes)", content.len())
            }
            Invocation::EditFile { path, .. } => format!("edit {path}"),
            Invocation::ListDir { path } => format!("list {path}"),
            Invocation::Glob { pattern, scope } => format!("find {pattern} in {}", scope.path),
            Invocation::Grep { query, scope } => {
                format!("search {} for {}", scope.path, query.pattern)
            }
            Invocation::RunCommand {
                command,
                background: true,
                ..
            } => format!("run `{command}` in the background"),
            Invocation::RunCommand { command, .. } => format!("run `{command}`"),
            Invocation::CommandOutput { id, .. } => format!("read the output of {id}"),
            Invocation::CommandStop { id } => format!("stop {id}"),
        }
    }

    pub async fn execute(&self, workspace: &Workspace) -> Result<String, ToolError> {
        match self {
            Invocation::ReadFile {
                path,
                offset,
                limit,
            } => files::read_file(workspace, path, *offset, *limit).await,
            Invocation::WriteFile { path, content } => {
                files::write_file(workspace, path, content).await
            }
            Invocation::EditFile {
                path,
                old_string,
                new_string,
            } => files::edit_file(workspace, path, old_string, new_string).await,
            Invocation::ListDir { path } => files::list_dir(workspace, path).await,
            Invocation::Glob { pattern, scope } => {
                let (workspace, pattern, scope) =
                    (workspace.clone(), pattern.clone(), scope.clone());
                blocking(move || search::glob_files(&workspace, &pattern, &scope)).await
            }
            Invocation::Grep { query, scope } => {
                let (workspace, query, scope) = (workspace.clone(), query.clone(), scope.clone());
                blocking(move || search::grep_files(&workspace, &query, &scope)).await
            }
            Invocation::RunCommand {
                command,
                background: true,
                ..
            } => {
                let id = workspace.processes().start(workspace, command)?;
                Ok(format!(
                    "started {id} in the background. Read its output with command_output \
                     {{\"id\": \"{id}\"}} and stop it with command_stop; it is stopped with the \
                     session."
                ))
            }
            Invocation::RunCommand {
                command, timeout_s, ..
            } => exec::run_command(workspace, command, Duration::from_secs(*timeout_s)).await,
            Invocation::CommandOutput { id, since } => workspace.processes().output(id, *since),
            Invocation::CommandStop { id } => workspace.processes().stop(id).await,
        }
    }
}

/// Runs a filesystem walk off the async runtime's threads.
async fn blocking(
    work: impl FnOnce() -> Result<String, ToolError> + Send + 'static,
) -> Result<String, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ToolError::Io(format!("the search failed: {error}")))?
}

/// Definitions of every built-in tool, as offered to the model.
pub fn specs() -> Vec<ToolSpec> {
    let path = json!({"type": "string", "description": "Path relative to the project root, or an absolute path inside any of the environment's folders"});
    vec![
        ToolSpec {
            name: "read_file".into(),
            description: "Read a UTF-8 text file. Lines are numbered; use offset (1-based line) and limit to page through large files.".into(),
            parameters: json!({"type": "object", "properties": {
                "path": path, "offset": {"type": "integer"}, "limit": {"type": "integer"}
            }, "required": ["path"]}),
        },
        ToolSpec {
            name: "write_file".into(),
            description: "Create or overwrite a file with the given content.".into(),
            parameters: json!({"type": "object", "properties": {
                "path": path, "content": {"type": "string"}
            }, "required": ["path", "content"]}),
        },
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace exactly one occurrence of old_string with new_string in a file. old_string must match exactly and be unique.".into(),
            parameters: json!({"type": "object", "properties": {
                "path": path, "old_string": {"type": "string"}, "new_string": {"type": "string"}
            }, "required": ["path", "old_string", "new_string"]}),
        },
        ToolSpec {
            name: "list_dir".into(),
            description: "List a directory's entries; directories end with /.".into(),
            parameters: json!({"type": "object", "properties": {"path": path}, "required": ["path"]}),
        },
        ToolSpec {
            name: "glob".into(),
            description: "Find files by name under a folder (default the project root), sorted by path. A pattern without / matches the file name at any depth (*.rs); one with / matches the path relative to the folder (src/**/*.ts); {a,b} gives alternatives. Skips .git and files .gitignore ignores unless include_ignored. Page with offset.".into(),
            parameters: json!({"type": "object", "properties": {
                "pattern": {"type": "string"},
                "path": {"type": "string", "description": "Folder to search; default the project root"},
                "max_results": {"type": "integer", "description": "Default 100, at most 1000"},
                "offset": {"type": "integer", "description": "Results to skip, for paging"},
                "include_ignored": {"type": "boolean", "description": "Also search files .gitignore ignores"}
            }, "required": ["pattern"]}),
        },
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents for a regular expression (Rust regex syntax) under a folder or in one file. Prints path:line:text per matching line, context lines as path-line-text. Skips .git, binary files, and files .gitignore ignores unless include_ignored. Page with offset.".into(),
            parameters: json!({"type": "object", "properties": {
                "pattern": {"type": "string", "description": "Regular expression"},
                "path": {"type": "string", "description": "Folder or file to search; default the project root"},
                "glob": {"type": "string", "description": "Only files whose names match, such as *.rs or *.{ts,tsx}"},
                "case_insensitive": {"type": "boolean"},
                "context": {"type": "integer", "description": "Lines shown before and after each match, at most 10"},
                "max_results": {"type": "integer", "description": "Matching lines to show; default 100, at most 1000"},
                "offset": {"type": "integer", "description": "Matching lines to skip, for paging"},
                "include_ignored": {"type": "boolean"}
            }, "required": ["pattern"]}),
        },
        ToolSpec {
            name: "run_command".into(),
            description: "Run a shell command in the project root and return its exit code, stdout, and stderr. With background: true (for servers and watchers) it returns a process id at once; read its output with command_output and end it with command_stop.".into(),
            parameters: json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "timeout_s": {"type": "integer", "description": "Default 120; not used in the background"},
                "background": {"type": "boolean", "description": "Start it and return its id without waiting"}
            }, "required": ["command"]}),
        },
        ToolSpec {
            name: "command_output".into(),
            description: "Read a background command's status and the output it wrote since the last read (or from byte since).".into(),
            parameters: json!({"type": "object", "properties": {
                "id": {"type": "string"},
                "since": {"type": "integer", "description": "Byte offset to read from; default where the last read stopped"}
            }, "required": ["id"]}),
        },
        ToolSpec {
            name: "command_stop".into(),
            description: "Stop a background command and every process it started.".into(),
            parameters: json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_parses_into_an_invocation() {
        let samples = [
            ("read_file", r#"{"path":"a"}"#),
            ("write_file", r#"{"path":"a","content":""}"#),
            (
                "edit_file",
                r#"{"path":"a","old_string":"x","new_string":"y"}"#,
            ),
            ("list_dir", r#"{"path":"."}"#),
            ("glob", r#"{"pattern":"*.rs"}"#),
            ("grep", r#"{"pattern":"fn main","context":2}"#),
            ("run_command", r#"{"command":"ls"}"#),
            ("command_output", r#"{"id":"p1"}"#),
            ("command_stop", r#"{"id":"p1"}"#),
        ];
        let names: Vec<String> = specs().into_iter().map(|spec| spec.name).collect();
        assert_eq!(names, samples.map(|(name, _)| name.to_string()));
        for (name, arguments) in samples {
            assert_eq!(
                Invocation::parse(name, arguments).unwrap().tool_name(),
                name
            );
        }
    }

    #[test]
    fn bad_calls_are_reported() {
        assert!(matches!(
            Invocation::parse("delete_everything", "{}"),
            Err(ToolError::UnknownTool(_))
        ));
        assert!(matches!(
            Invocation::parse("write_file", r#"{"path":"a"}"#),
            Err(ToolError::Invalid(_))
        ));
        assert_eq!(
            Invocation::parse("run_command", r#"{"command":"x","timeout_s":99999}"#).unwrap(),
            Invocation::RunCommand {
                command: "x".into(),
                timeout_s: 3600,
                background: false
            }
        );
        assert!(matches!(
            Invocation::parse("grep", r#"{"pattern":"x","max_results":99999,"context":50}"#).unwrap(),
            Invocation::Grep { query, scope }
                if query.context == 10 && scope.max_results == 1000 && scope.path == "."
        ));
    }

    fn path_of(name: &str, arguments: &str) -> String {
        match Invocation::parse(name, arguments).unwrap() {
            Invocation::ReadFile { path, .. } | Invocation::ListDir { path } => path,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unescaped_windows_paths_are_read_as_written() {
        let spaced = r"D:\Git Repositories\Imperium-AI";
        assert_eq!(
            path_of("list_dir", &format!(r#"{{"path": "{spaced}"}}"#)),
            spaced
        );
        // `\n` and `\t` are JSON escapes, but a path never holds a newline or a tab.
        assert_eq!(
            path_of("read_file", r#"{"path": "D:\new\tools.rs", "limit": 5}"#),
            r"D:\new\tools.rs"
        );
        // Properly escaped arguments are untouched.
        assert_eq!(
            path_of("read_file", r#"{"path": "D:\\Git Repositories\\a.txt"}"#),
            r"D:\Git Repositories\a.txt"
        );
        // Content keeps its escapes: only the unparseable backslash is read literally.
        assert_eq!(
            Invocation::parse(
                "write_file",
                r#"{"path": "C:\Temp\a.txt", "content": "one\ntwo"}"#
            )
            .unwrap(),
            Invocation::WriteFile {
                path: r"C:\Temp\a.txt".into(),
                content: "one\ntwo".into()
            }
        );
    }
}
