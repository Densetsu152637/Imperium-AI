//! Error messages that clients tell apart by their text. They are plain strings on the wire
//! (a JSON-RPC error carries no machine-readable kind for them), so the daemon, the engine and
//! the clients share the wording from here, and the desktop pins the same literals in its own
//! tests.

/// What an error message starts with when a mod install or update asked for trust and the
/// daemon did not grant it (`<this>: <why>`); the command-line client then installs the mod in
/// its sandbox instead.
pub const TRUST_NOT_GRANTED: &str = "trust was not granted";

/// What an error message starts with when a message was sent to a subagent that is no longer
/// running (`<this> "<id>" in this session`); the desktop then sends it as a follow-up.
pub const NO_RUNNING_AGENT: &str = "no running agent";
