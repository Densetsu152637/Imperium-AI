//! Daemon side of `run_script` (code mode, stage 1a of `docs/plans/code-mode.md`).
//!
//! [`Launcher`] starts one confined worker per program and returns a [`ScriptRun`], which the
//! engine drives with [`ScriptRun::next`] and answers with `resume_*`. The worker is untrusted:
//! the driver accepts only the events of plan C3 and kills the worker on anything else, answers
//! every `OsCall` with an error, counts every suspension against the program's budget and kills
//! the worker when its program ends. The Windows launch (token, job, labels) is stage 1b; until
//! then [`Launcher::start`] reports the worker unavailable there.

mod driver;

pub use driver::{Call, Launcher, Limits, ScriptError, ScriptEvent, ScriptRun};
pub use monty_types::MontyObject;
