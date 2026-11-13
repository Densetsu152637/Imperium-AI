//! Hooks: commands run at points in a session's life, receiving a JSON payload on stdin.
//!
//! `tool.before` and `prompt.submit` hooks can block: exit code 2 (stderr is the reason), or
//! exit 0 with `{"decision": "block" | "deny", "reason": "..."}` on stdout. Other events only
//! observe. A hook that fails or times out is logged and does not block.
//!
//! A hook is a command or an HTTP endpoint (`url`): the same JSON is POSTed to it, a 2xx answer
//! may carry the same `decision`, and anything else is a failure. HTTP hooks come only from the
//! user's configuration, go to `https` URLs (or `http` on this machine), never follow a
//! redirect, and take secrets only from the keychain (`${secret:name}` in a header value).
//!
//! Claude Code `hooks.json` files load unchanged: `PreToolUse` is `tool.before`, and so on,
//! and Claude Code tool names in matchers (`Bash`, `Edit|Write`) match Imperium's tools.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// What an HTTP hook gets by default: it holds up the session's turn while it runs.
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest any configured hook may run.
const MAX_TIMEOUT: Duration = Duration::from_secs(300);
/// Most of an HTTP hook's answer that is read.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HookEvent {
    SessionStart,
    PromptSubmit,
    ToolBefore,
    ToolAfter,
    AgentSpawn,
    AgentComplete,
    SessionEnd,
    TurnStart,
    TurnEnd,
    PermissionRequested,
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
            HookEvent::SessionEnd => "session.end",
            HookEvent::TurnStart => "turn.start",
            HookEvent::TurnEnd => "turn.end",
            HookEvent::PermissionRequested => "permission.requested",
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
            "session.end" | "SessionEnd" => HookEvent::SessionEnd,
            "turn.start" => HookEvent::TurnStart,
            "turn.end" => HookEvent::TurnEnd,
            "permission.requested" => HookEvent::PermissionRequested,
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
    /// Run through the shell (`sh -c`, or `cmd /C` on Windows); empty for an HTTP hook.
    pub command: String,
    /// Where an HTTP hook POSTs its payload instead of running `command`.
    pub http: Option<HttpTarget>,
    pub timeout: Duration,
    /// Working directory; the project folder when unset.
    pub cwd: Option<PathBuf>,
    pub origin: String,
}

/// An HTTP hook's endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpTarget {
    /// `https://...`, or `http://` to localhost, 127.0.0.0/8, or `::1`.
    pub url: String,
    /// Extra request headers; a value may hold `${secret:name}`, read from the keychain when
    /// the hook runs.
    pub headers: BTreeMap<String, String>,
}

type SecretReader = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Reads a keychain secret by name; only HTTP hook headers use it.
#[derive(Clone)]
pub struct Secrets(Arc<SecretReader>);

impl Secrets {
    pub fn new(read: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Secrets(Arc::new(read))
    }
}

impl fmt::Debug for Secrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secrets(..)")
    }
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
    /// The command, or the URL without its query, for logs.
    fn describe(&self) -> String {
        match &self.http {
            Some(target) => target
                .url
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .to_string(),
            None => self.command.clone(),
        }
    }

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
    secrets: Option<Secrets>,
}

impl Hooks {
    pub fn new(hooks: Vec<Hook>) -> Self {
        Hooks {
            hooks,
            secrets: None,
        }
    }

    /// Where HTTP hooks' `${secret:name}` header values come from (the keychain). Without it
    /// such a hook fails.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Secrets) -> Self {
        self.secrets = Some(secrets);
        self
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
            let ran = match &hook.http {
                Some(target) => run_http(hook, target, &input, self.secrets.as_ref()).await,
                None => run_one(hook, &input, cwd).await,
            };
            match ran {
                Ok(Verdict::Block(reason)) if event.can_block() => return Verdict::Block(reason),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(hook = %hook.describe(), event = event.as_str(), %error, "hook failed");
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
        Some(0) => Ok(decision(&String::from_utf8_lossy(&output.stdout))),
        Some(2) => Ok(Verdict::Block(if stderr.is_empty() {
            "blocked by a hook".into()
        } else {
            stderr
        })),
        code => Err(format!("exited with {code:?}: {stderr}")),
    }
}

/// A hook's answer: `{"decision": "block" | "deny", "reason": "..."}` blocks; anything else allows.
fn decision(text: &str) -> Verdict {
    let decision: Option<Value> = serde_json::from_str(text.trim()).ok();
    let blocked = decision
        .as_ref()
        .and_then(|d| d.get("decision"))
        .and_then(Value::as_str)
        .is_some_and(|d| d == "block" || d == "deny");
    if !blocked {
        return Verdict::Allow;
    }
    let reason = decision
        .as_ref()
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str)
        .unwrap_or("blocked by a hook");
    Verdict::Block(reason.to_string())
}

/// Checks an HTTP hook's URL: `https`, or `http` to this machine, and no credentials in it.
pub fn check_url(text: &str) -> Result<reqwest::Url, String> {
    let url =
        reqwest::Url::parse(text).map_err(|error| format!("the hook URL is invalid: {error}"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "the hook URL must not hold credentials; use a header with ${secret:name}".into(),
        );
    }
    let host = url.host_str().ok_or("the hook URL has no host")?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" => Ok(url),
        "http" if loopback => Ok(url),
        "http" => Err("an HTTP hook must use https, or http to this machine".into()),
        other => Err(format!("an HTTP hook cannot use {other}:")),
    }
}

/// Replaces each `${secret:name}` in a header value with the keychain secret of that name.
fn expand_secrets(value: &str, secrets: Option<&Secrets>) -> Result<String, String> {
    const OPEN: &str = "${secret:";
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let end = after
            .find('}')
            .ok_or("a header has an unterminated ${secret:...}")?;
        let name = &after[..end];
        let read = secrets.ok_or("a header uses ${secret:...} but no keychain is available")?;
        let secret = (read.0)(name).ok_or_else(|| format!("no keychain secret named {name}"))?;
        out.push_str(&secret);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// POSTs the payload to an HTTP hook. The request goes to the configured URL only: redirects
/// are not followed, so a server cannot send the payload (or a secret header) on to another
/// host. The whole exchange, body included, is bounded by the hook's timeout.
async fn run_http(
    hook: &Hook,
    target: &HttpTarget,
    input: &Value,
    secrets: Option<&Secrets>,
) -> Result<Verdict, String> {
    let url = check_url(&target.url)?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(hook.timeout)
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .body(input.to_string());
    for (name, value) in &target.headers {
        request = request.header(name.as_str(), expand_secrets(value, secrets)?);
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| error.without_url().to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(if status.is_redirection() {
            format!("answered {status}; HTTP hooks do not follow redirects")
        } else {
            format!("answered {status}")
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| error.without_url().to_string())?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_RESPONSE_BYTES {
            return Err("the answer is larger than 64 KiB".into());
        }
    }
    Ok(decision(&String::from_utf8_lossy(&body)))
}

/// `[[hooks]]` entries in Imperium configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct ConfiguredHook {
    pub event: String,
    #[serde(default)]
    pub matcher: String,
    /// The command to run; or `url`, not both.
    #[serde(default)]
    pub command: String,
    /// An HTTP hook's endpoint (see [`check_url`]).
    #[serde(default)]
    pub url: Option<String>,
    /// An HTTP hook's extra headers; values may use `${secret:name}`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_s: Option<u64>,
}

impl ConfiguredHook {
    pub fn into_hook(self, origin: &str) -> Result<Hook, String> {
        let event = HookEvent::parse(&self.event)
            .ok_or_else(|| format!("unknown hook event {:?}", self.event))?;
        let http = match (self.url, self.command.trim().is_empty()) {
            (Some(url), true) => {
                check_url(&url)?;
                for name in self.headers.keys() {
                    if matches!(
                        name.to_ascii_lowercase().as_str(),
                        "host" | "content-length" | "content-type"
                    ) {
                        return Err(format!("the header {name} cannot be set on a hook"));
                    }
                }
                Some(HttpTarget {
                    url,
                    headers: self.headers,
                })
            }
            (Some(_), false) => return Err("a hook has a command or a url, not both".into()),
            (None, true) => return Err("a hook needs a command or a url".into()),
            (None, false) => None,
        };
        let default = if http.is_some() {
            DEFAULT_HTTP_TIMEOUT
        } else {
            DEFAULT_TIMEOUT
        };
        Ok(Hook {
            event,
            matcher: self.matcher,
            command: self.command,
            http,
            timeout: self
                .timeout_s
                .map_or(default, |s| Duration::from_secs(s).min(MAX_TIMEOUT)),
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
                if entry.kind == "http" {
                    skipped.push(format!(
                        "{origin}: HTTP hooks come only from the user's configuration"
                    ));
                    continue;
                }
                if entry.kind != "command" || entry.command.is_empty() {
                    skipped.push(format!("{origin}: only command hooks are supported"));
                    continue;
                }
                hooks.push(Hook {
                    event,
                    matcher: group.matcher.clone(),
                    command: entry.command.replace("${CLAUDE_PLUGIN_ROOT}", &root),
                    http: None,
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
            http: None,
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

    #[test]
    fn the_new_events_have_names() {
        for (name, event) in [
            ("session.end", HookEvent::SessionEnd),
            ("SessionEnd", HookEvent::SessionEnd),
            ("turn.start", HookEvent::TurnStart),
            ("turn.end", HookEvent::TurnEnd),
            ("permission.requested", HookEvent::PermissionRequested),
        ] {
            assert_eq!(HookEvent::parse(name), Some(event), "{name}");
        }
        for event in [
            HookEvent::SessionEnd,
            HookEvent::TurnStart,
            HookEvent::TurnEnd,
            HookEvent::PermissionRequested,
        ] {
            assert_eq!(HookEvent::parse(event.as_str()), Some(event));
            // Observers only: none of them can hold a turn, a tool, or an approval.
            assert!(!event.can_block());
        }
    }

    #[test]
    fn http_hook_urls_are_https_or_this_machine() {
        for good in [
            "https://hooks.example.com/x?y=1",
            "http://localhost:8080/h",
            "http://127.0.0.1:9/h",
            "http://[::1]:9/h",
        ] {
            assert!(check_url(good).is_ok(), "{good}");
        }
        for bad in [
            "http://example.com/h",
            "http://10.0.0.5/h",
            "ftp://localhost/h",
            "https://user:pw@example.com/h",
            "not a url",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    fn configured(url: Option<&str>, command: &str) -> ConfiguredHook {
        ConfiguredHook {
            event: "turn.start".into(),
            matcher: String::new(),
            command: command.into(),
            url: url.map(str::to_string),
            headers: BTreeMap::new(),
            timeout_s: Some(2),
        }
    }

    #[test]
    fn a_hook_is_a_command_or_a_url() {
        assert!(configured(None, "true").into_hook("t").is_ok());
        let http = configured(Some("http://127.0.0.1:1/h"), "")
            .into_hook("t")
            .unwrap();
        assert!(http.http.is_some());
        assert!(
            configured(Some("http://127.0.0.1:1/h"), "true")
                .into_hook("t")
                .is_err()
        );
        assert!(configured(None, " ").into_hook("t").is_err());
        assert!(
            configured(Some("http://example.com/h"), "")
                .into_hook("t")
                .is_err()
        );
        let mut host = configured(Some("http://127.0.0.1:1/h"), "");
        host.headers.insert("Host".into(), "evil".into());
        assert!(host.into_hook("t").is_err());
        let (hooks, skipped) = parse_hooks_json(
            r#"{"hooks": {"Stop": [{"hooks": [{"type": "http", "url": "https://x.example/h"}]}]}}"#,
            std::path::Path::new("/p"),
            "plugin:p",
        )
        .unwrap();
        assert!(hooks.is_empty());
        assert_eq!(skipped.len(), 1);
    }

    /// What the test server saw: the path, the Authorization header, and the body.
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(String, Option<String>, String)>>>;

    fn note(seen: &Seen, path: &str, headers: &axum::http::HeaderMap, body: String) {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        seen.lock().unwrap().push((path.to_string(), auth, body));
    }

    async fn serve() -> (String, Seen) {
        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode};
        use axum::routing::post;

        async fn observe(
            State(seen): State<Seen>,
            headers: HeaderMap,
            body: String,
        ) -> &'static str {
            note(&seen, "/observe", &headers, body);
            "ok"
        }
        async fn deny(
            State(seen): State<Seen>,
            headers: HeaderMap,
            body: String,
        ) -> axum::Json<Value> {
            note(&seen, "/deny", &headers, body);
            axum::Json(json!({"decision": "block", "reason": "the server said no"}))
        }
        async fn redirect(
            State(seen): State<Seen>,
            headers: HeaderMap,
            body: String,
        ) -> (StatusCode, [(&'static str, &'static str); 1]) {
            note(&seen, "/redirect", &headers, body);
            (StatusCode::TEMPORARY_REDIRECT, [("location", "/elsewhere")])
        }
        async fn elsewhere(State(seen): State<Seen>, headers: HeaderMap, body: String) {
            note(&seen, "/elsewhere", &headers, body);
        }
        async fn slow() {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        let seen = Seen::default();
        let app = axum::Router::new()
            .route("/observe", post(observe))
            .route("/deny", post(deny))
            .route("/redirect", post(redirect))
            .route("/elsewhere", post(elsewhere))
            .route("/slow", post(slow))
            .with_state(std::sync::Arc::clone(&seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), seen)
    }

    fn http_hook(event: &str, url: String, authorization: Option<&str>, timeout_s: u64) -> Hook {
        let mut hook = configured(Some(&url), "");
        hook.event = event.into();
        hook.timeout_s = Some(timeout_s);
        if let Some(value) = authorization {
            hook.headers.insert("Authorization".into(), value.into());
        }
        hook.into_hook("test").unwrap()
    }

    #[tokio::test]
    async fn http_hooks_post_the_payload_with_a_keychain_secret_and_can_block() {
        let (base, seen) = serve().await;
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::new(|name| (name == "hook-token").then(|| "s3cret".to_string()));
        let hooks = Hooks::new(vec![
            http_hook(
                "turn.start",
                format!("{base}/observe"),
                Some("Bearer ${secret:hook-token}"),
                5,
            ),
            http_hook("tool.before", format!("{base}/deny"), None, 5),
        ])
        .with_secrets(secrets);
        let verdict = hooks
            .run(
                HookEvent::TurnStart,
                None,
                &json!({"session_id": "s1", "prompt": "hi"}),
                dir.path(),
            )
            .await;
        assert_eq!(verdict, Verdict::Allow);
        {
            let seen = seen.lock().unwrap();
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].0, "/observe");
            assert_eq!(seen[0].1.as_deref(), Some("Bearer s3cret"));
            let body: Value = serde_json::from_str(&seen[0].2).unwrap();
            assert_eq!(body["hook_event"], "turn.start");
            assert_eq!(body["session_id"], "s1");
        }
        let verdict = hooks
            .run(
                HookEvent::ToolBefore,
                Some("run_command"),
                &json!({"tool_input": {}}),
                dir.path(),
            )
            .await;
        assert_eq!(verdict, Verdict::Block("the server said no".into()));
    }

    #[tokio::test]
    async fn an_http_hook_does_not_follow_a_redirect() {
        let (base, seen) = serve().await;
        let dir = tempfile::tempdir().unwrap();
        let hooks = Hooks::new(vec![http_hook(
            "tool.before",
            format!("{base}/redirect"),
            None,
            5,
        )]);
        let verdict = hooks
            .run(HookEvent::ToolBefore, Some("x"), &json!({}), dir.path())
            .await;
        // A failed hook does not block, and the redirect target was never asked.
        assert_eq!(verdict, Verdict::Allow);
        let paths: Vec<String> = seen.lock().unwrap().iter().map(|s| s.0.clone()).collect();
        assert_eq!(paths, vec!["/redirect".to_string()]);
    }

    #[tokio::test]
    async fn a_slow_http_hook_times_out() {
        let (base, _seen) = serve().await;
        let dir = tempfile::tempdir().unwrap();
        let hooks = Hooks::new(vec![http_hook(
            "tool.before",
            format!("{base}/slow"),
            None,
            1,
        )]);
        let started = std::time::Instant::now();
        let verdict = hooks
            .run(HookEvent::ToolBefore, Some("x"), &json!({}), dir.path())
            .await;
        assert_eq!(verdict, Verdict::Allow);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn a_secret_header_without_a_secret_sends_nothing() {
        let (base, seen) = serve().await;
        let dir = tempfile::tempdir().unwrap();
        let make = || {
            Hooks::new(vec![http_hook(
                "turn.end",
                format!("{base}/observe"),
                Some("Bearer ${secret:nope}"),
                5,
            )])
        };
        for hooks in [make().with_secrets(Secrets::new(|_| None)), make()] {
            let verdict = hooks
                .run(HookEvent::TurnEnd, None, &json!({}), dir.path())
                .await;
            assert_eq!(verdict, Verdict::Allow);
        }
        assert!(seen.lock().unwrap().is_empty());
    }
}
