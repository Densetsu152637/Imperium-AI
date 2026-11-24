//! The answer of an OS presence check: the user at this machine proving they are there with a
//! fingerprint, PIN or password (plugin approvals, check 6, docs/06-extensibility.md#plugin-marketplaces).
//! The daemon raises the check itself; the macOS and Windows mechanisms live in this crate's
//! `unsafe`-allowed modules, the Linux one (polkit over D-Bus) in `imperium-core`.

/// What an OS presence check answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// The user proved they are there.
    Passed,
    /// The operating system offers no check here (none set up, no authentication agent, no
    /// window session); the reason.
    Unavailable(String),
    /// The user refused or cancelled, the check failed, or nobody answered in time; the reason.
    Refused(String),
}
