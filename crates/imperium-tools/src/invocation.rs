use std::time::Duration;

use imperium_api::ToolSpec;
use serde::Deserialize;
use serde_json::json;

use crate::exec::DEFAULT_TIMEOUT_S;
use crate::{Workspace, exec, files};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    #[error("unknown tool {0:?}")]
    UnknownTool(String),
    #[error("invalid arguments: {0}")]
    Invalid(String),
    #[error("{0} is outside the workspace")]
    OutsideWorkspace(String),
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
    RunCommand {
        command: String,
        timeout_s: u64,
    },
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
}

fn parse<T: for<'de> Deserialize<'de>>(arguments: &str) -> Result<T, ToolError> {
    let arguments = if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    };
    serde_json::from_str(arguments).map_err(|error| ToolError::Invalid(error.to_string()))
}

impl Invocation {
    /// Parses a model's tool call (name and JSON arguments).
    pub fn parse(name: &str, arguments: &str) -> Result<Self, ToolError> {
        Ok(match name {
            "read_file" => {
                let args: PathArgs = parse(arguments)?;
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
                let args: PathArgs = parse(arguments)?;
                Invocation::ListDir { path: args.path }
            }
            "run_command" => {
                let args: CommandArgs = parse(arguments)?;
                Invocation::RunCommand {
                    command: args.command,
                    timeout_s: args.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S).clamp(1, 3600),
                }
            }
            other => return Err(ToolError::UnknownTool(other.to_string())),
        })
    }

    pub fn access(&self) -> Access {
        match self {
            Invocation::ReadFile { .. } | Invocation::ListDir { .. } => Access::Read,
            Invocation::WriteFile { .. } | Invocation::EditFile { .. } => Access::Write,
            Invocation::RunCommand { .. } => Access::Execute,
        }
    }

    pub fn tool_name(&self) -> &'static str {
        match self {
            Invocation::ReadFile { .. } => "read_file",
            Invocation::WriteFile { .. } => "write_file",
            Invocation::EditFile { .. } => "edit_file",
            Invocation::ListDir { .. } => "list_dir",
            Invocation::RunCommand { .. } => "run_command",
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
            Invocation::RunCommand { command, .. } => format!("run `{command}`"),
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
            Invocation::RunCommand { command, timeout_s } => {
                exec::run_command(workspace, command, Duration::from_secs(*timeout_s)).await
            }
        }
    }
}

/// Definitions of every built-in tool, as offered to the model.
pub fn specs() -> Vec<ToolSpec> {
    let path = json!({"type": "string", "description": "Path relative to the project root"});
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
            name: "run_command".into(),
            description: "Run a shell command in the project root and return its exit code, stdout, and stderr.".into(),
            parameters: json!({"type": "object", "properties": {
                "command": {"type": "string"},
                "timeout_s": {"type": "integer", "description": "Default 120"}
            }, "required": ["command"]}),
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
            ("run_command", r#"{"command":"ls"}"#),
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
                timeout_s: 3600
            }
        );
    }
}
