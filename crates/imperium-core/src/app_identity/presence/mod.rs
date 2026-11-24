//! Check 6 of who may approve a plugin: the OS presence check
//! ([plan](../../../../../docs/plans/plugin-marketplace.md#presence-check-per-os),
//! docs/06-extensibility.md#plugin-marketplaces).
//!
//! The daemon raises the check itself, after the approval's challenge was answered and before
//! anything is recorded; no caller can report a result, since nothing in `plugins.approve` carries
//! one. The answer decides the kind a release build records:
//!
//! - passed: `counted`;
//! - unavailable (the OS offers no check without setup: no polkit action file or agent, no
//!   window session, Windows Hello not set up): `dialog-only`, the dialog alone;
//! - refused, cancelled, failed or timed out: nothing is recorded.
//!
//! Per OS: polkit `CheckAuthorization` on Linux (`linux.rs`), LocalAuthentication on macOS and
//! Windows Hello on Windows (both in `imperium-ipc`, which allows the `unsafe` calls); another
//! platform has none, so its approvals are `dialog-only`.

// A debug build records `dev` without a presence check, so outside its tests nothing here runs.
#![cfg_attr(debug_assertions, allow(dead_code))]

use std::time::Duration;

pub(crate) use imperium_ipc::Presence;

use crate::plugin_records::ApprovalKind;

#[cfg(target_os = "linux")]
mod linux;

/// How long the user has to answer the prompt. The desktop app waits five minutes for
/// `plugins.approve`, so the check ends first.
pub(crate) const ANSWER_TIME: Duration = Duration::from_secs(240);
/// The longest plugin id shown in a prompt.
const MAX_SHOWN: usize = 120;

/// Raises the OS presence check. Production uses [`System`]; tests inject their own.
pub(crate) trait PresenceProvider: Send + Sync {
    /// Asks the user at this machine to prove they are there before `plugin` is approved, and
    /// waits for the answer.
    fn check(&self, plugin: &str) -> Presence;
}

/// The operating system's own check.
pub(crate) struct System;

impl PresenceProvider for System {
    fn check(&self, plugin: &str) -> Presence {
        let plugin = shown(plugin);
        #[cfg(target_os = "linux")]
        {
            linux::check(&plugin, ANSWER_TIME)
        }
        #[cfg(target_os = "macos")]
        {
            imperium_ipc::device_owner_check(&reason(&plugin), ANSWER_TIME)
        }
        #[cfg(windows)]
        {
            imperium_ipc::hello_check(&reason(&plugin), ANSWER_TIME)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            Presence::Unavailable(format!(
                "no presence check is built for this platform (plugin {plugin})"
            ))
        }
    }
}

/// The kind a release build records for `presence`'s answer, or why nothing is recorded.
pub(crate) fn kind_for(presence: Presence) -> Result<ApprovalKind, String> {
    match presence {
        Presence::Passed => Ok(ApprovalKind::Counted),
        Presence::Unavailable(why) => {
            tracing::info!("no OS presence check here ({why}): the approval is dialog-only");
            Ok(ApprovalKind::DialogOnly)
        }
        Presence::Refused(why) => Err(format!(
            "the presence check did not pass ({why}): nothing was recorded"
        )),
    }
}

/// What the prompt says the approval is for.
#[cfg_attr(target_os = "linux", allow(dead_code))]
fn reason(plugin: &str) -> String {
    format!("approve the Imperium plugin {plugin} so its hooks and servers can run")
}

/// `plugin` as the prompt shows it: control and invisible characters written out, at most
/// [`MAX_SHOWN`] characters.
fn shown(plugin: &str) -> String {
    let mut text: String = plugin
        .chars()
        .flat_map(|c| {
            if c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}') {
                format!("\\u{{{:x}}}", u32::from(c)).chars().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect();
    if text.chars().count() > MAX_SHOWN {
        text = text.chars().take(MAX_SHOWN).collect::<String>() + "…";
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_answer_maps_to_its_kind() {
        assert_eq!(kind_for(Presence::Passed), Ok(ApprovalKind::Counted));
        assert_eq!(
            kind_for(Presence::Unavailable("no agent".into())),
            Ok(ApprovalKind::DialogOnly)
        );
        let refused = kind_for(Presence::Refused("cancelled".into())).unwrap_err();
        assert!(refused.contains("cancelled"), "{refused}");
        assert!(refused.contains("nothing was recorded"), "{refused}");
    }

    #[test]
    fn the_prompt_shows_the_plugin_id_written_out_and_bounded() {
        assert_eq!(shown("p@m"), "p@m");
        assert_eq!(shown("p\u{202e}x@m\n"), "p\\u{202e}x@m\\u{a}");
        let long = "a".repeat(500);
        assert_eq!(shown(&long).chars().count(), MAX_SHOWN + 1);
        assert!(reason("p@m").contains("p@m"));
    }
}
