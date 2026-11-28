//! Imperium's own record of the frames a Ratio turn opens.
//!
//! `open_frame` checks a child against this record of its parent (the live record at the
//! parent's path), never against the `FrameCtx` Ratio passes in. The parent's depth comes from
//! the record; the child's kind comes from its origin as Imperium verified it against what it
//! loaded and started:
//!
//! - a harness by package, IR hash and key, named as its key says;
//! - a block only at a `with` site of a program Imperium loaded, its path naming that site;
//! - an entry only as the harness entry Imperium itself started (from the turn or
//!   `run_harness`), once per start, at the session's path, as a record on top of the
//!   session's, which it never replaces or closes;
//! - the session's own frame is Imperium's and never opened.
//!
//! One live frame per path, and a child's path is its parent's plus exactly one element
//! (branch elements, then `<harness>@s<site>#<occurrence>`), in canonical digits.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use ratio::frame::{FrameCtx, FrameKind, FrameOrigin, Scope, check_within};
use ratio::host::{DeniedBy, HostError};

#[derive(Clone, Debug)]
struct Record {
    scope: Arc<Scope>,
    /// Opened through `open_frame` (the session's own record is not, and is never closed).
    opened: bool,
}

#[derive(Debug, Default)]
struct Records {
    /// The live records by path: a stack, so a started entry's record sits on the session's.
    open: HashMap<String, Vec<Record>>,
    /// Every package Imperium loaded (imports included), by name and IR hash.
    packages: HashSet<(Arc<str>, [u8; 32])>,
    /// Every harness Imperium loaded, as the origin its frames carry.
    harnesses: HashSet<FrameOrigin>,
    /// Every `with` block site of the programs Imperium loaded.
    blocks: HashSet<([u8; 32], u32)>,
    /// Harness entries Imperium started and whose frame has not opened yet (each opens once).
    started: HashSet<FrameOrigin>,
}

#[derive(Debug)]
pub(crate) struct FrameRecords {
    inner: Mutex<Records>,
}

fn attenuation(message: String) -> HostError {
    HostError::Denied {
        by: DeniedBy::Attenuation,
        message,
    }
}

/// Canonical digits: no sign, no leading zero unless the number is 0.
fn number(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))
}

/// The element a child's path adds to its parent's: zero or more branch elements
/// (`c<site>#<occurrence>.b<index>`), then `<name>@s<site>#<occurrence>`. `None` unless the
/// child's path is exactly that.
fn child_element<'a>(parent: &str, child: &'a str) -> Option<(&'a str, u32)> {
    let rest = child.strip_prefix(parent)?.strip_prefix('/')?;
    let mut parts: Vec<&str> = rest.split('/').collect();
    let last = parts.pop()?;
    for branch in parts {
        let (construct, index) = branch.split_once(".b")?;
        let (site, occurrence) = construct.strip_prefix('c')?.split_once('#')?;
        if !(number(site) && number(occurrence) && number(index)) {
            return None;
        }
    }
    let (name, tail) = last.rsplit_once("@s")?;
    let (site, occurrence) = tail.split_once('#')?;
    if name.is_empty() || name.contains(['@', '#']) || !number(site) || !number(occurrence) {
        return None;
    }
    Some((name, site.parse().ok()?))
}

/// The harness's name, as its key (`<module path>::<name>`) gives it.
fn name_of(key: &str) -> &str {
    key.rsplit("::").next().unwrap_or("")
}

impl FrameRecords {
    /// The session's own frame, at `root`, with the session's scope.
    pub fn new(root: &str, scope: Scope) -> FrameRecords {
        let mut records = Records::default();
        records.open.insert(
            root.to_owned(),
            vec![Record {
                scope: Arc::new(scope),
                opened: false,
            }],
        );
        FrameRecords {
            inner: Mutex::new(records),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Records> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Imperium pins what it loaded (`Prepared::loaded`).
    pub fn load(
        &self,
        packages: impl IntoIterator<Item = (Arc<str>, [u8; 32])>,
        harnesses: impl IntoIterator<Item = FrameOrigin>,
        blocks: impl IntoIterator<Item = ([u8; 32], u32)>,
    ) {
        let mut records = self.lock();
        records.packages.extend(packages);
        records.harnesses.extend(harnesses);
        records.blocks.extend(blocks);
    }

    /// Imperium starts a harness entry (a turn's, or `run_harness`'s): its frame may open once.
    pub fn start_entry(&self, origin: FrameOrigin) {
        self.lock().started.insert(origin);
    }

    /// The scope of the live frame at `path`, by Imperium's record.
    pub fn scope_of(&self, path: &str) -> Option<Arc<Scope>> {
        self.lock()
            .open
            .get(path)
            .and_then(|stack| stack.last())
            .map(|r| Arc::clone(&r.scope))
    }

    pub fn open(&self, parent: &FrameCtx, child: &FrameCtx) -> Result<(), HostError> {
        let mut records = self.lock();
        // The parent from Imperium's own record, never from the context passed in.
        let Some(parent_scope) = records
            .open
            .get(&*parent.path)
            .and_then(|stack| stack.last())
            .map(|r| Arc::clone(&r.scope))
        else {
            return Err(attenuation(format!(
                "Imperium has no record of the frame `{}`",
                parent.path
            )));
        };
        let live_at_path = records.open.get(&*child.path).map_or(0, Vec::len);
        // The kind from the origin as Imperium verified it, never from `child.kind` alone.
        let kind = match &child.origin {
            FrameOrigin::Session => None,
            FrameOrigin::Entry { key, .. } => {
                let ok = records.started.contains(&child.origin)
                    && *child.harness == *name_of(key)
                    && child.path == parent.path
                    // On the session's record alone: never a second entry frame at once.
                    && live_at_path == 1;
                ok.then_some(FrameKind::Entry)
            }
            FrameOrigin::Block { site } => {
                let ok = records.blocks.contains(site)
                    && &*child.harness == "with"
                    && child_element(&parent.path, &child.path)
                        .is_some_and(|(name, s)| name == "with" && s == site.1);
                ok.then_some(FrameKind::Block)
            }
            FrameOrigin::Harness {
                package,
                entry_hash,
                key,
            } => {
                let ok = records
                    .packages
                    .contains(&(Arc::clone(package), *entry_hash))
                    && records.harnesses.contains(&child.origin)
                    && *child.harness == *name_of(key)
                    && child_element(&parent.path, &child.path)
                        .is_some_and(|(name, _)| name == &*child.harness);
                ok.then_some(FrameKind::Harness)
            }
        };
        let Some(kind) = kind else {
            return Err(attenuation(format!(
                "the frame `{}` has an origin Imperium did not load or start, or a path that \
                 does not fit it",
                child.path
            )));
        };
        let parent_ok = match kind {
            FrameKind::Entry => {
                child.parent.is_none() || child.parent.as_deref() == Some(&*parent.path)
            }
            FrameKind::Harness | FrameKind::Block => child.parent.as_deref() == Some(&*parent.path),
        };
        // One live frame per path (the started entry on the session's record excepted).
        if kind != child.kind || !parent_ok || (kind != FrameKind::Entry && live_at_path > 0) {
            return Err(attenuation(format!(
                "the frame `{}` does not fit its kind, or its path is live",
                child.path
            )));
        }
        // Depth and every other dimension against the recorded parent.
        if let Err(dimension) = check_within(&parent_scope, &child.scope, kind) {
            return Err(attenuation(format!(
                "the frame `{}` is wider than its parent in `{}`",
                child.path,
                dimension.name()
            )));
        }
        if kind == FrameKind::Entry {
            // Once per start.
            records.started.remove(&child.origin);
        }
        records
            .open
            .entry(child.path.to_string())
            .or_default()
            .push(Record {
                scope: Arc::clone(&child.scope),
                opened: true,
            });
        Ok(())
    }

    pub fn close(&self, path: &str) {
        let mut records = self.lock();
        // Only a record `open_frame` made is closed; the session's own stays.
        let empty = match records.open.get_mut(path) {
            Some(stack) if stack.last().is_some_and(|r| r.opened) => {
                stack.pop();
                stack.is_empty()
            }
            _ => false,
        };
        if empty {
            records.open.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::child_element;

    #[test]
    fn a_child_path_is_its_parents_and_one_element_in_canonical_digits() {
        assert_eq!(child_element("turn", "turn/h@s1#0"), Some(("h", 1)));
        assert_eq!(child_element("turn", "turn/c4#0.b1/h@s1#0"), Some(("h", 1)));
        for bad in [
            "turn/a@s9#0/h@s1#0",
            "elsewhere/h@s1#0",
            "turn/h@s1#0/",
            "turn/h@sx#0",
            "turn/c4#0/h@s1#0",
            "turn/h@s01#0",
            "turn/c04#0.b1/h@s1#0",
            "turnx/h@s1#0",
        ] {
            assert_eq!(child_element("turn", bad), None, "{bad}");
        }
    }
}
