//! The sandboxed worker behind `run_script` (code mode, stage 1 of `docs/plans/code-mode.md`).
//!
//! One worker process serves exactly one program. It confines itself before it reads a single
//! request (Linux and macOS; on Windows it checks the confinement the daemon applied at launch,
//! [`confine`]) and fails closed when it cannot ([`EXIT_CONFINEMENT_FAILED`]); only then does it
//! run its protocol loop over `monty::MontyRun` ([`serve`]) on stdin and stdout. The daemon side
//! of the protocol is the `imperium-script` crate.

pub mod confine;
pub mod options;
pub mod serve;

/// Exit status when OS confinement could not be applied: BSD `sysexits.h` `EX_OSERR`. The worker
/// has read nothing from stdin by then, so no program ran.
pub const EXIT_CONFINEMENT_FAILED: u8 = 71;

/// Exit status for a command line the worker does not accept: `EX_USAGE`.
pub const EXIT_USAGE: u8 = 64;
