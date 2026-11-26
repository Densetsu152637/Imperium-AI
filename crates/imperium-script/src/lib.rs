//! Daemon side of `run_script` (code mode, stage 1 of `docs/plans/code-mode.md`).
//!
//! [`Launcher`] starts one confined worker per program and returns a [`ScriptRun`], which the
//! engine drives with [`ScriptRun::next_until`] and answers with `resume_*`. The worker is
//! untrusted: the driver accepts only the events of plan C3 and kills the worker on anything
//! else, answers every `OsCall` with an error, counts every suspension against the program's
//! budget and kills the worker when its program ends.
//!
//! Confinement per OS (plan C1): on Linux and macOS the worker confines itself before it reads a
//! program; the driver adds a cgroup v2 memory limit on Linux when the daemon runs in a delegated
//! cgroup, and a resident-memory watchdog on macOS, which has no OS memory limit. On Windows the
//! driver launches the worker confined (the `windows` module): suspended, with a restricted
//! Low-integrity token, in a job object, with only its pipes inherited, and only once the
//! daemon's folders carry their `NO_READ_UP` label (`windows::labels`).
//!
//! The engine depends on the [`ScriptRunner`] and [`ScriptProgram`] traits; [`Launcher`] is the
//! real runner and [`FakeRunner`] replays scripted events for engine tests. A program that names
//! `eval` or `exec` is refused before a worker starts ([`dynamic_code`]); every call a program
//! makes is checked again at call time with a [`HandleTable`].

#[cfg(target_os = "linux")]
mod cgroup;
mod driver;
mod fake;
mod handles;
mod locate;
mod process;
mod runner;
mod scan;
#[cfg(target_os = "macos")]
mod watchdog;
#[cfg(windows)]
pub mod windows;

pub use driver::{Call, Launcher, Limits, ScriptError, ScriptEvent, ScriptRun};
pub use fake::{FakeAnswer, FakeRunner, FakeStep};
pub use handles::{Admission, HandleTable};
pub use locate::{WORKER_NAME, worker_beside};
pub use monty_types::MontyObject;
pub use runner::{BoxFuture, ScriptProgram, ScriptRunner};
pub use scan::dynamic_code;
