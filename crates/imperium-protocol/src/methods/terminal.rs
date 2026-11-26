//! `terminal.*`: the desktop app's terminals (stage 2b of `docs/plans/terminal-file-panes.md`).
//!
//! Every method is answered only to the installed desktop app on the local socket (decision D2);
//! anyone else, a paired phone included, gets `-32005`. Terminal bytes are base64. Output and
//! changes go only to connections attached to the terminal, never to other clients.

use super::*;

/// `terminal.open`: starts a terminal in the session's working folder; the caller is attached.
#[derive(Debug)]
pub struct TerminalOpen;

impl Method for TerminalOpen {
    const NAME: &'static str = "terminal.open";
    type Params = TerminalOpenParams;
    type Result = TerminalInfo;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalOpenParams {
    pub session_id: String,
    /// Clamped to 1 to 1000.
    pub cols: u16,
    /// Clamped to 1 to 500.
    pub rows: u16,
}

/// `terminal.list`: a session's terminals, oldest first.
#[derive(Debug)]
pub struct TerminalList;

impl Method for TerminalList {
    const NAME: &'static str = "terminal.list";
    type Params = TerminalListParams;
    type Result = TerminalListResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalListParams {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalListResult {
    pub terminals: Vec<TerminalInfo>,
}

/// `terminal.attach`: the output kept from `since` (all of it when absent), and the caller is
/// attached; later output arrives as `terminal.output`.
#[derive(Debug)]
pub struct TerminalAttach;

impl Method for TerminalAttach {
    const NAME: &'static str = "terminal.attach";
    type Params = TerminalAttachParams;
    type Result = TerminalAttachResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalAttachParams {
    pub terminal_id: String,
    /// The `seq` to replay from: the end of what the caller already has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub since: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalAttachResult {
    pub info: TerminalInfo,
    /// The kept output from `seq` on, base64.
    pub replay: String,
    /// The `seq` of the first byte of `replay`.
    #[ts(type = "number")]
    pub seq: u64,
    /// Output from `since` was dropped from the 1 MiB ring before `seq`.
    pub truncated: bool,
}

/// `terminal.detach`: the caller stops receiving the terminal's output; the terminal keeps
/// running (an hour with nothing attached ends it).
#[derive(Debug)]
pub struct TerminalDetach;

impl Method for TerminalDetach {
    const NAME: &'static str = "terminal.detach";
    type Params = TerminalRef;
    type Result = Accepted;
}

/// `terminal.close`: ends the terminal's processes and removes it.
#[derive(Debug)]
pub struct TerminalClose;

impl Method for TerminalClose {
    const NAME: &'static str = "terminal.close";
    type Params = TerminalRef;
    type Result = Accepted;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalRef {
    pub terminal_id: String,
}

/// `terminal.input`: keystrokes for the shell.
#[derive(Debug)]
pub struct TerminalInput;

impl Method for TerminalInput {
    const NAME: &'static str = "terminal.input";
    type Params = TerminalInputParams;
    type Result = Accepted;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalInputParams {
    pub terminal_id: String,
    /// The bytes, base64; at most 64 KiB once decoded.
    pub data: String,
}

/// `terminal.resize`: the terminal's new size, clamped; the last resize wins.
#[derive(Debug)]
pub struct TerminalResize;

impl Method for TerminalResize {
    const NAME: &'static str = "terminal.resize";
    type Params = TerminalResizeParams;
    type Result = Accepted;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalResizeParams {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

/// Whether a terminal's shell still runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TerminalState {
    Running,
    Exited,
}

/// One terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalInfo {
    pub terminal_id: String,
    pub session_id: String,
    /// The shell's name, until the terminal sets a title of its own.
    pub title: String,
    pub cwd: String,
    pub shell: String,
    pub cols: u16,
    pub rows: u16,
    pub state: TerminalState,
    /// How the shell exited, once it has.
    pub exit_code: Option<u32>,
    /// When it was opened, in Unix milliseconds.
    #[ts(type = "number")]
    pub created_at: u64,
}

/// Notification of output, to the connections attached to the terminal only: `seq` and `data`,
/// or `lagged` when the connection fell more than 1 MiB behind (it re-attaches with `since`).
pub const TERMINAL_OUTPUT: &str = "terminal.output";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalOutputParams {
    pub terminal_id: String,
    /// The `seq` (running byte offset) of the first byte of `data`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub seq: Option<u64>,
    /// The bytes, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<String>,
    /// Output was dropped for this connection: nothing more comes until it attaches again.
    #[serde(default)]
    pub lagged: bool,
}

/// Notification that a terminal changed (its shell exited), to its attached connections only.
pub const TERMINAL_CHANGED: &str = "terminal.changed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TerminalChangedParams {
    pub info: TerminalInfo,
}
