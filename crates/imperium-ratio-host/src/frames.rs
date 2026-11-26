//! Imperium's own record of the frames a Ratio turn opens.
//!
//! `open_frame` checks a child against this record of its parent (looked up by the parent's
//! path), never against the `FrameCtx` Ratio passes in. The parent's depth and kind come from
//! the record; the child's kind comes from Imperium's own knowledge where it has any (a path
//! where Imperium itself started a harness is a harness frame, however it is presented).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use ratio::frame::{FrameCtx, FrameKind, Scope, check_within};
use ratio::host::{DeniedBy, HostError};

#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub scope: Arc<Scope>,
}

#[derive(Debug, Default)]
struct Records {
    open: HashMap<String, Record>,
    /// Paths where Imperium itself started a harness (its harness loads and `run_harness`).
    harnesses: HashSet<String>,
}

#[derive(Debug)]
pub(crate) struct FrameRecords {
    session_root: String,
    session_scope: Arc<Scope>,
    inner: Mutex<Records>,
}

fn attenuation(message: String) -> HostError {
    HostError::Denied {
        by: DeniedBy::Attenuation,
        message,
    }
}

impl FrameRecords {
    /// The session's own frame, at `root`, with the session's scope.
    pub fn new(root: &str, scope: Scope) -> FrameRecords {
        let scope = Arc::new(scope);
        let mut records = Records::default();
        records.open.insert(
            root.to_owned(),
            Record {
                scope: Arc::clone(&scope),
            },
        );
        FrameRecords {
            session_root: root.to_owned(),
            session_scope: scope,
            inner: Mutex::new(records),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Records> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Imperium starts a harness at `path` (a harness load or `run_harness`).
    pub fn harness_started(&self, path: &str) {
        self.lock().harnesses.insert(path.to_owned());
    }

    /// The scope Imperium recorded for the frame at `path`.
    pub fn scope_of(&self, path: &str) -> Option<Arc<Scope>> {
        self.lock().open.get(path).map(|r| Arc::clone(&r.scope))
    }

    pub fn open(&self, parent: &FrameCtx, child: &FrameCtx) -> Result<(), HostError> {
        let mut records = self.lock();
        // The parent from Imperium's own record, never from the context passed in.
        let Some(record) = records.open.get(&*parent.path).cloned() else {
            return Err(attenuation(format!(
                "Imperium has no record of the frame `{}`",
                parent.path
            )));
        };
        // The kind from Imperium's own knowledge where it has any: only harnesses Imperium
        // loaded or ran are harness frames by its record. Elsewhere it can only check that
        // Ratio's assertion is consistent (see the spike's trait-change list).
        let known_harness = records.harnesses.contains(&*child.path);
        let kind = if known_harness {
            FrameKind::Harness
        } else {
            child.kind
        };
        let named_block = &*child.harness == "with";
        let parent_ok = match kind {
            FrameKind::Entry => child.path == parent.path,
            FrameKind::Harness | FrameKind::Block => child.parent.as_deref() == Some(&*parent.path),
        };
        if kind != child.kind || (kind == FrameKind::Block) != named_block || !parent_ok {
            return Err(attenuation(format!(
                "the frame `{}` does not fit its kind by Imperium's record",
                child.path
            )));
        }
        // Depth and every other dimension against the recorded parent.
        if let Err(dimension) = check_within(&record.scope, &child.scope, kind) {
            return Err(attenuation(format!(
                "the frame `{}` is wider than its parent in `{}`",
                child.path,
                dimension.name()
            )));
        }
        records.open.insert(
            child.path.to_string(),
            Record {
                scope: Arc::clone(&child.scope),
            },
        );
        Ok(())
    }

    pub fn close(&self, path: &str) {
        let mut records = self.lock();
        if path == self.session_root {
            // An entry frame closed: the session's own frame stays open for the next turn.
            records.open.insert(
                path.to_owned(),
                Record {
                    scope: Arc::clone(&self.session_scope),
                },
            );
        } else {
            records.open.remove(path);
        }
    }
}
