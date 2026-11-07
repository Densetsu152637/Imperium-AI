//! Hooks: commands run at points in a session's life, receiving a JSON payload on stdin.
//!
//! `tool.before` and `prompt.submit` hooks can block: exit code 2 (stderr is the reason), or
//! exit 0 with `{"decision": "block" | "deny", "reason": "..."}` on stdout. Other events only
//! observe. A hook that fails or times out is logged and does not block.
//!
//! Claude Code `hooks.json` files load unchanged: `PreToolUse` is `tool.before`, and so on,
//! and Claude Code tool names in matchers (`Bash`, `Edit|Write`) match Imperium's tools.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HookEvent {
    SessionStart,
    PromptSubmit,
    ToolBefore,
    ToolAfter,
    AgentSpawn,
    AgentComplete,
}

impl HookEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session.start",
            HookEvent::PromptSubmit => "prompt.submit",
            HookEvent::ToolBefore => "tool.before",
            HookEvent::ToolAfter => "tool.after",
            HookEvent::AgentSpawn => "agent.spawn",
            HookEvent::AgentComplete => "agent.complete",
        }
    }

    /// Imperium names, and Claude Code's for hooks.json files.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "session.start" | "SessionStart" => HookEvent::SessionStart,
            "prompt.submit" | "UserPromptSubmit" => HookEvent::PromptSubmit,
            "tool.before" | "PreToolUse" => HookEvent::ToolBefore,
            "tool.after" | "PostToolUse" => HookEvent::ToolAfter,
            "agent.spawn" => HookEvent::AgentSpawn,
            "agent.complete" | "SubagentStop" | "Stop" => HookEvent::AgentComplete,
            _ => return None,
        })
    }

    fn can_block(self) -> bool {
        matches!(self, HookEvent::ToolBefore | HookEvent::PromptSubmit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hook {
    pub event: HookEvent,
    /// Tool names this hook applies to, `|`-separated; empty or `*` matches every tool.
    pub matcher: String,
    /// Run through the shell (`sh -c`, or `cmd /C` on Windows).
    pub command: String,
    pub timeout: Duration,
    /// Working directory; the project folder when unset.
    pub cwd: Option<PathBuf>,
    pub origin: String,
}

/// What blocking hooks decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Block(String),
}

/// Claude Code's names for Imperium's built-in tools, so matchers written for it work.
fn tool_aliases(tool: &str) -> &'static [&'static str] {
    match tool {
        "run_command" => &["Bash"],
        "read_file" => &["Read"],
        "write_file" => &["Write"],
        "edit_file" => &["Edit", "MultiEdit"],
        "list_dir" => &["LS", "Glob"],
        "spawn_agent" => &["Task", "Agent"],
        _ => &[],
    }
}

impl Hook {
    fn matches(&self, tool: Option<&str>) -> bool {
        let Some(tool) = tool else {
            return true;
        };
        let pattern = self.matcher.trim();
        pattern.is_empty()
            || pattern == "*"
            || pattern
                .split('|')
                .map(str::trim)
                .any(|name| name == tool || tool_aliases(tool).contains(&name))
    }
}

/// The hooks in effect for a session.
#[derive(Debug, Clone, Default)]
pub struct Hooks {
    hooks: Vec<Hook>,
}

impl Hooks {
    pub fn new(hooks: Vec<Hook>) -> Self {
        Hooks { hooks }
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Runs the hooks for `event` (and `tool`, for tool events) in order. For blocking events
    /// the first block wins and later hooks do not run.
    pub async fn run(
        &self,
        event: HookEvent,
        tool: Option<&str>,
        payload: &Value,
        project_dir: &std::path::Path,
    ) -> Verdict {
        let mut input = payload.clone();
        if let Some(object) = input.as_object_mut() {
            object.insert("hook_event".into(), event.as_str().into());
            if let Some(tool) = tool {
                object.insert("tool_name".into(), tool.into());
            }
        }
        for hook in self
            .hooks
            .iter()
            .filter(|hook| hook.event == event && hook.matches(tool))
        {
            let cwd = hook.cwd.as_deref().unwrap_or(project_dir);
            match run_one(hook, &input, cwd).await {
                Ok(Verdict::Block(reason)) if event.can_block() => return Verdict::Block(reason),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(hook = %hook.command, event = event.as_str(), %error, "hook failed");
                }
            }
        }
        Verdict::Allow
    }
}

async fn run_one(hook: &Hook, input: &Value, cwd: &std::path::Path) -> Result<Verdict, String> {
    let mut child = imperium_tools::shell(&hook.command)
        .current_dir(cwd)
        .env("IMPERIUM_PROJECT_DIR", cwd)
        .env("CLAUDE_PROJECT_DIR", cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| error.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        // A hook that ignores its input may exit before reading it; that is not an error.
        let _ = stdin.write_all(input.to_string().as_bytes()).await;
    }
    let output = tokio::time::timeout(hook.timeout, child.wait_with_output())
        .await
        .map_err(|_| format!("timed out after {} s", hook.timeout.as_secs()))?
        .map_err(|error| error.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let decision: Option<Value> = serde_json::from_str(stdout.trim()).ok();
            let blocked = decision
                .as_ref()
                .and_then(|d| d.get("decision"))
                .and_then(Value::as_str)
                .is_some_and(|d| d == "block" || d == "deny");
            Ok(if blocked {
                let reason = decision
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("blocked by a hook");
                Verdict::Block(reason.to_string())
            } else {
                Verdict::Allow
            })
        }
        Some(2) => Ok(Verdict::Block(if stderr.is_empty() {
            "blocked by a hook".into()
        } else {
            stderr
        })),
        code => Err(format!("exited with {code:?}: {stderr}")),
    }
}

/// `[[hooks]]` entries in Imperium configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfiguredHook {
    pub event: String,
    #[serde(default)]
    pub matcher: String,
    pub command: String,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

impl ConfiguredHook {
    pub fn into_hook(self, origin: &str) -> Result<Hook, String> {
        let event = HookEvent::parse(&self.event)
            .ok_or_else(|| format!("unknown hook event {:?}", self.event))?;
        Ok(Hook {
            event,
            matcher: self.matcher,
            command: self.command,
            timeout: self.timeout_s.map_or(DEFAULT_TIMEOUT, Duration::from_secs),
            cwd: None,
            origin: origin.to_string(),
        })
    }
}

/// Parses a Claude Code `hooks.json` (`{"hooks": {"PreToolUse": [{"matcher", "hooks": [...]}]}}`).
/// `${CLAUDE_PLUGIN_ROOT}` is replaced with `plugin_root`. Unsupported events are reported.
pub fn parse_hooks_json(
    text: &str,
    plugin_root: &std::path::Path,
    origin: &str,
) -> Result<(Vec<Hook>, Vec<String>), String> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        hooks: BTreeMap<String, Vec<Group>>,
    }
    #[derive(Deserialize)]
    struct Group {
        #[serde(default)]
        matcher: String,
        #[serde(default)]
        hooks: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        #[serde(rename = "type", default)]
        kind: String,
        #[serde(default)]
        command: String,
        #[serde(default)]
        timeout: Option<u64>,
    }
    let file: File = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let root = plugin_root.display().to_string();
    let mut hooks = Vec::new();
    let mut skipped = Vec::new();
    for (name, groups) in file.hooks {
        let Some(event) = HookEvent::parse(&name) else {
            skipped.push(format!("{origin}: hook event {name} is not supported"));
            continue;
        };
        for group in groups {
            for entry in group.hooks {
                if entry.kind != "command" || entry.command.is_empty() {
                    skipped.push(format!("{origin}: only command hooks are supported"));
                    continue;
                }
                hooks.push(Hook {
                    event,
                    matcher: group.matcher.clone(),
                    command: entry.command.replace("${CLAUDE_PLUGIN_ROOT}", &root),
                    timeout: entry.timeout.map_or(DEFAULT_TIMEOUT, Duration::from_secs),
                    cwd: None,
                    origin: origin.to_string(),
                });
            }
        }
    }
    Ok((hooks, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook(event: HookEvent, matcher: &str, command: &str) -> Hook {
        Hook {
            event,
            matcher: matcher.into(),
            command: command.into(),
            timeout: Duration::from_secs(10),
            cwd: None,
            origin: "test".into(),
        }
    }

    #[test]
    fn matchers_accept_claude_code_tool_names() {
        let h = hook(HookEvent::ToolBefore, "Edit|Write", "true");
        assert!(h.matches(Some("write_file")));
        assert!(h.matches(Some("edit_file")));
        assert!(!h.matches(Some("run_command")));
        assert!(hook(HookEvent::ToolBefore, "", "true").matches(Some("anything")));
        assert!(hook(HookEvent::ToolBefore, "mcp__x__y", "true").matches(Some("mcp__x__y")));
    }

    #[tokio::test]
    async fn blocking_hooks_deny_with_a_reason_and_observers_cannot_block() {
        let dir = tempfile::tempdir().unwrap();
        let deny_exit = if cfg!(windows) {
            "echo no commands here 1>&2 & exit /b 2"
        } else {
            "echo no commands here >&2; exit 2"
        };
        let deny_json = if cfg!(windows) {
            r#"echo {"decision": "block", "reason": "json says no"}"#
        } else {
            r#"echo '{"decision": "block", "reason": "json says no"}'"#
        };
        let hooks = Hooks::new(vec![
            hook(HookEvent::ToolBefore, "Bash", deny_exit),
            hook(HookEvent::ToolBefore, "Write", deny_json),
            hook(HookEvent::ToolAfter, "", deny_exit),
        ]);
        let payload = json!({"tool_input": {}});
        assert_eq!(
            hooks
                .run(
                    HookEvent::ToolBefore,
                    Some("run_command"),
                    &payload,
                    dir.path()
                )
                .await,
            Verdict::Block("no commands here".into())
        );
        assert_eq!(
            hooks
                .run(
                    HookEvent::ToolBefore,
                    Some("write_file"),
                    &payload,
                    dir.path()
                )
                .await,
            Verdict::Block("json says no".into())
        );
        assert_eq!(
            hooks
                .run(
                    HookEvent::ToolBefore,
                    Some("read_file"),
                    &payload,
                    dir.path()
                )
                .await,
            Verdict::Allow
        );
        assert_eq!(
            hooks
                .run(
                    HookEvent::ToolAfter,
                    Some("run_command"),
                    &payload,
                    dir.path()
                )
                .await,
            Verdict::Allow
        );
    }

    #[tokio::test]
    async fn hooks_receive_the_payload_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("seen.json");
        let command = if cfg!(windows) {
            format!("more > \"{}\"", out.display())
        } else {
            format!("cat > '{}'", out.display())
        };
        let hooks = Hooks::new(vec![hook(HookEvent::PromptSubmit, "", &command)]);
        let verdict = hooks
            .run(
                HookEvent::PromptSubmit,
                None,
                &json!({"prompt": "hi"}),
                dir.path(),
            )
            .await;
        assert_eq!(verdict, Verdict::Allow);
        let seen: Value =
            serde_json::from_str(std::fs::read_to_string(&out).unwrap().trim()).unwrap();
        assert_eq!(seen["prompt"], "hi");
        assert_eq!(seen["hook_event"], "prompt.submit");
    }

    #[test]
    fn claude_code_hooks_json_loads() {
        let text = r#"{"hooks": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/check.sh", "timeout": 5}]}],
            "PreCompact": [{"hooks": [{"type": "command", "command": "x"}]}],
            "PostToolUse": [{"hooks": [{"type": "prompt", "prompt": "x"}]}]
        }}"#;
        let (hooks, skipped) =
            parse_hooks_json(text, std::path::Path::new("/plugins/p"), "plugin:p").unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].command, "/plugins/p/check.sh");
        assert_eq!(hooks[0].timeout, Duration::from_secs(5));
        assert_eq!(skipped.len(), 2);
    }
}
