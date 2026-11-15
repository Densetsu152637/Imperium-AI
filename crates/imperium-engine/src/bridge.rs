//! Running a session's own tools for an outside caller, outside a turn: the daemon side of
//! `imperium mcp-serve --tools` (docs/20-claude-harness.md).
//!
//! A call goes to the session's root agent as a command and runs through the same path as a
//! call the agent's model makes: the launcher's tool limits, the agent's permission profile and
//! local-files-only lock, the tool's own validation, the permission rules and the safety floor,
//! approval (an ask is a `permission_requested` event the user answers), hooks, and the
//! journal. No model is called, so no budget is spent. This file holds what stands beside that
//! path: which tools may be bridged, and where finished results wait for the caller.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Notify;

use imperium_protocol::tools as contract;

/// Tools that only read; the bridge offers them unless the caller asks for nothing.
pub const READ_TOOLS: &[&str] = &[
    "read_file",
    "list_dir",
    "glob",
    "grep",
    "memory_search",
    "memory_read",
    "web_search",
    "web_fetch",
];

/// Tools that change files, run commands or save memory; the bridge runs them only for a caller
/// that was allowed writes, and each still asks under the session's permission mode.
pub const WRITE_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "run_command",
    "command_output",
    "command_stop",
    "memory_remember",
];

/// A tool the bridge can run.
#[derive(Debug, Clone, PartialEq)]
pub struct BridgeTool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: Value,
    /// Whether it changes things (needs writes allowed).
    pub writes: bool,
}

/// Every tool the bridge can run, read tools first, as models are offered them.
pub fn tools() -> Vec<BridgeTool> {
    let mut specs = imperium_tools::specs();
    specs.extend([
        crate::agent::memory_search_spec(),
        crate::agent::memory_read_spec(),
        crate::agent::memory_remember_spec(),
    ]);
    for definition in [contract::web_search(), contract::web_fetch()] {
        specs.push(imperium_api::ToolSpec {
            name: definition.name.into(),
            description: definition.description.into(),
            parameters: definition.parameters,
        });
    }
    let mut tools: Vec<BridgeTool> = specs
        .into_iter()
        .filter_map(|spec| {
            let writes = if READ_TOOLS.contains(&spec.name.as_str()) {
                false
            } else if WRITE_TOOLS.contains(&spec.name.as_str()) {
                true
            } else {
                return None;
            };
            Some(BridgeTool {
                name: spec.name,
                description: spec.description,
                parameters: spec.parameters,
                writes,
            })
        })
        .collect();
    tools.sort_by_key(|tool| tool.writes);
    tools
}

/// Why a tool is not bridged, or `Ok` when it may be.
pub fn check(name: &str, allow_write: bool) -> Result<(), String> {
    if READ_TOOLS.contains(&name) {
        return Ok(());
    }
    if WRITE_TOOLS.contains(&name) {
        return if allow_write {
            Ok(())
        } else {
            Err(format!(
                "{name} changes files or runs commands, and writes are not allowed for this bridge"
            ))
        };
    }
    Err(format!("{name} is not a tool the bridge runs"))
}

/// How a bridged call ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeOutcome {
    pub ok: bool,
    pub output: String,
}

/// What a wait for a bridged call found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgeWait {
    /// The call ended; its result is handed over once.
    Done(BridgeOutcome),
    /// Still running (or waiting for a permission answer).
    Pending,
    /// No such call, or its result was already taken (or dropped to make room).
    Unknown,
}

/// Finished results kept for callers that have not collected them yet.
const KEPT: usize = 64;

#[derive(Debug, Default)]
struct State {
    pending: HashSet<String>,
    done: HashMap<String, BridgeOutcome>,
    order: VecDeque<String>,
}

/// Bridged calls of one session: the ones running and the results waiting to be collected.
#[derive(Debug, Default)]
pub struct BridgeResults {
    next: AtomicU64,
    state: Mutex<State>,
    changed: Notify,
}

impl BridgeResults {
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Registers a call and returns its id.
    pub fn begin(&self) -> String {
        let id = format!("b{}", self.next.fetch_add(1, Ordering::SeqCst) + 1);
        self.state().pending.insert(id.clone());
        id
    }

    /// Forgets a call that never reached the session.
    pub fn abandon(&self, id: &str) {
        self.state().pending.remove(id);
    }

    /// Records how a call ended and wakes the waiters.
    pub fn finish(&self, id: &str, ok: bool, output: String) {
        {
            let mut state = self.state();
            state.pending.remove(id);
            state
                .done
                .insert(id.to_string(), BridgeOutcome { ok, output });
            state.order.push_back(id.to_string());
            while state.order.len() > KEPT {
                if let Some(oldest) = state.order.pop_front() {
                    state.done.remove(&oldest);
                }
            }
        }
        self.changed.notify_waiters();
    }

    fn take(&self, id: &str) -> BridgeWait {
        let mut state = self.state();
        if let Some(outcome) = state.done.remove(id) {
            state.order.retain(|kept| kept != id);
            return BridgeWait::Done(outcome);
        }
        if state.pending.contains(id) {
            BridgeWait::Pending
        } else {
            BridgeWait::Unknown
        }
    }

    /// Waits up to `limit` for the call to end and takes its result.
    pub async fn wait(&self, id: &str, limit: Duration) -> BridgeWait {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            // Registered before the check, so a result that lands between the two is not lost.
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.take(id) {
                BridgeWait::Pending => {}
                other => return other,
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return self.take(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_bridged_always_and_writes_only_when_allowed() {
        assert!(check("read_file", false).is_ok());
        assert!(check("web_fetch", false).is_ok());
        assert!(check("write_file", false).is_err());
        assert!(check("run_command", false).is_err());
        assert!(check("write_file", true).is_ok());
        // Delegation, questions to the user and the like are never bridged.
        for name in ["spawn_agent", "ask_user", "start_session", "update_plan"] {
            assert!(check(name, true).is_err(), "{name}");
        }
    }

    #[test]
    fn every_listed_tool_has_a_definition() {
        let listed = tools();
        for name in READ_TOOLS.iter().chain(WRITE_TOOLS) {
            let tool = listed.iter().find(|tool| tool.name == *name);
            let tool = tool.unwrap_or_else(|| panic!("no definition for {name}"));
            assert_eq!(tool.writes, WRITE_TOOLS.contains(name));
        }
        assert_eq!(listed.len(), READ_TOOLS.len() + WRITE_TOOLS.len());
    }

    #[tokio::test]
    async fn a_result_is_handed_over_once() {
        let results = BridgeResults::default();
        let id = results.begin();
        assert_eq!(
            results.wait(&id, Duration::from_millis(10)).await,
            BridgeWait::Pending
        );
        results.finish(&id, true, "text".into());
        assert_eq!(
            results.wait(&id, Duration::from_millis(10)).await,
            BridgeWait::Done(BridgeOutcome {
                ok: true,
                output: "text".into()
            })
        );
        assert_eq!(
            results.wait(&id, Duration::from_millis(10)).await,
            BridgeWait::Unknown
        );
        assert_eq!(
            results.wait("nope", Duration::from_millis(10)).await,
            BridgeWait::Unknown
        );
    }
}
