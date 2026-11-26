//! Tools: the cancellation-class table, the Ratio arguments to Imperium's JSON arguments, the
//! permission decision through `imperium_policy`, and the secret-path rules.

use std::collections::BTreeMap;
use std::time::Duration;

use imperium_api::{PermissionMode, ToolCheck};
use imperium_policy::permissions::{CallKind, Decision, Target, decide_call, glob, safety_floor};
use ratio::Value;
use ratio::host::{CancelClass, MAX_DRAIN, ToolClass, ToolInvocation};

/// Imperium's table of tool classes. Only Imperium's own configuration sets an entry: a
/// provider's or an MCP server's read-only claim never makes a tool `abort`, and a tool not in
/// the table is `drain`, bounded at [`MAX_DRAIN`].
#[derive(Clone, Debug)]
pub struct ClassTable {
    classes: BTreeMap<String, ToolClass>,
}

impl Default for ClassTable {
    /// Imperium's built-in tools.
    fn default() -> Self {
        let mut table = ClassTable {
            classes: BTreeMap::new(),
        };
        for read in ["read_file", "list_dir", "glob", "grep", "search_files"] {
            table.set(read, ToolClass::abort(false));
        }
        for remote in ["web_fetch", "web_search"] {
            table.set(remote, ToolClass::abort(true));
        }
        for write in ["write_file", "edit_file"] {
            table.set(write, ToolClass::drain(Duration::from_secs(60)));
        }
        table.set("run_command", ToolClass::drain(Duration::from_secs(600)));
        table
    }
}

impl ClassTable {
    /// Sets a tool's class; a `drain` class without a bound, or over [`MAX_DRAIN`], is bounded
    /// at [`MAX_DRAIN`].
    pub fn set(&mut self, tool: &str, class: ToolClass) {
        let class = match class.class {
            CancelClass::Abort => class,
            CancelClass::Drain => ToolClass {
                max_running: Some(class.max_running.unwrap_or(MAX_DRAIN).min(MAX_DRAIN)),
                ..class
            },
        };
        self.classes.insert(tool.to_owned(), class);
    }

    pub fn class(&self, tool: &str) -> ToolClass {
        self.classes
            .get(tool)
            .copied()
            .unwrap_or(ToolClass::UNLISTED)
    }

    pub fn names(&self) -> Vec<String> {
        self.classes.keys().cloned().collect()
    }
}

/// The secret-holding file patterns of Imperium's safety floor (`imperium_policy`'s private
/// `FLOOR_PATHS`, the subset that names secrets): the default `ratio.secret_paths`.
pub const DEFAULT_SECRET_PATHS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    "id_ecdsa*",
    ".npmrc",
    ".pypirc",
    ".netrc",
    "secrets.enc",
];

/// Whether `path` matches one of `patterns`, as a whole path or by its file name.
pub fn matches_secret_path(patterns: &[String], path: &str) -> bool {
    let path = path.replace('\\', "/");
    let name = path.rsplit('/').next().unwrap_or(&path);
    let fold = cfg!(any(windows, target_os = "macos"));
    patterns
        .iter()
        .any(|p| glob(p, &path, fold) || glob(p, name, fold))
}

fn to_json(v: &Value) -> serde_json::Value {
    if let Some(t) = v.as_text() {
        return serde_json::Value::from(t);
    }
    if let Some(i) = v.as_int() {
        return serde_json::Value::from(i);
    }
    if let Some(b) = v.as_bool() {
        return serde_json::Value::from(b);
    }
    if let Some(f) = v.as_float() {
        return serde_json::Number::from_f64(f).map_or(serde_json::Value::Null, Into::into);
    }
    // Records, lists and maps: the spike does not map them (an adapter gap, not a trait one).
    serde_json::Value::Null
}

/// The JSON arguments Imperium's providers take: `tools.call_tool` and `tools.mcp_call` pass
/// their `arguments` text through; a built-in's own call passes its arguments by name.
pub fn arguments_json(call: &ToolInvocation) -> String {
    if matches!(&*call.builtin, "tools.call_tool" | "tools.mcp_call") {
        return call
            .arg("arguments")
            .and_then(Value::as_text)
            .unwrap_or("{}")
            .to_owned();
    }
    let object: serde_json::Map<String, serde_json::Value> = call
        .args
        .iter()
        .map(|(name, value)| (name.to_string(), to_json(value)))
        .collect();
    serde_json::Value::Object(object).to_string()
}

/// What the call does, for Imperium's permission mode.
pub fn call_kind(tool: &str, check: &ToolCheck) -> CallKind {
    match tool {
        "read_file" | "list_dir" | "glob" | "grep" | "search_files" => CallKind::Read,
        "write_file" | "edit_file" => CallKind::Edit,
        "run_command" => CallKind::Execute,
        _ if check.command.is_some() => CallKind::Execute,
        _ => CallKind::Other { asks: check.ask },
    }
}

/// Imperium's decision for a call, from its mode and the safety floor (rules are not wired in
/// the spike).
pub fn decide(
    mode: PermissionMode,
    kind: CallKind,
    check: &ToolCheck,
    call: &ToolInvocation,
    root: &std::path::Path,
) -> Decision {
    let target = match kind {
        CallKind::Execute => Target::Command(
            check
                .command
                .clone()
                .or_else(|| {
                    call.arg("command")
                        .and_then(Value::as_text)
                        .map(str::to_owned)
                })
                .unwrap_or_default(),
        ),
        CallKind::Edit => {
            let path = call
                .arg("path")
                .and_then(Value::as_text)
                .unwrap_or_default();
            Target::Path {
                absolute: root.join(path).to_string_lossy().replace('\\', "/"),
                relative: Some(path.replace('\\', "/")),
            }
        }
        CallKind::Read | CallKind::Other { .. } => Target::Other,
    };
    decide_call(mode, kind, None, safety_floor(kind, &target))
}
