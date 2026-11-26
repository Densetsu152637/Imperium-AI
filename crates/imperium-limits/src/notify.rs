//! Linux: registers a unit's inotify descriptor with Tokio's reactor, so the supervisor wakes
//! when `memory.events` or `pids.events` change (plan L3, "Notices").
//!
//! `AsyncFd::register` is `unsafe` because Tokio cannot check that the descriptor stays open and
//! the same while registered. This module, and only this one on Linux, allows `unsafe` for that
//! one call.
#![allow(unsafe_code)]

use std::os::fd::OwnedFd;

use tokio::io::unix::AsyncFd;

/// Registers `fd` (non-blocking), or `None` when there is no reactor to register it with.
pub(crate) fn register(fd: OwnedFd) -> Option<AsyncFd<OwnedFd>> {
    // SAFETY: the `AsyncFd` takes ownership of `fd`, an open descriptor nothing else holds, and
    // an `OwnedFd` returns the same descriptor from every `as_raw_fd` until it is dropped.
    unsafe { AsyncFd::register(fd) }.ok()
}
