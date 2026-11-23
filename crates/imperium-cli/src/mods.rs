//! `imperium mods install` and `imperium mods update`: installing a UI mod from a git URL or a
//! folder after the user has seen, and approved, the permissions it requests. The daemon does
//! the work (`mods.inspect`, then `mods.install` with the hash and permissions shown here);
//! nothing in the mod runs.

use std::io::{IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, bail};
use imperium_protocol::methods::{
    ModInspect, ModInspectParams, ModInstall, ModInstallParams, ModList, ModListParams, ModPreview,
};

use crate::{block_on, connect};

/// What each permission lets the mod's frame ask the app for.
const PERMISSIONS: [(&str, &str); 4] = [
    (
        "telemetry:read",
        "receive telemetry events (calls, tokens, cost)",
    ),
    (
        "sessions:read",
        "read your sessions, their agents and raw events",
    ),
    (
        "pets:read",
        "list your Pixel Pets and read their animations",
    ),
    (
        "storage",
        "keep its own settings in the app (up to 256 KiB)",
    ),
];

fn describe(permission: &str) -> &'static str {
    PERMISSIONS
        .iter()
        .find(|(name, _)| *name == permission)
        .map_or("(unknown to this version)", |(_, text)| *text)
}

pub fn install(
    source: String,
    path: Option<String>,
    revision: Option<String>,
    sha256: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    let source = absolute_folder(source);
    block_on(async {
        let (mut client, _) = connect().await?;
        let preview = client
            .call::<ModInspect>(ModInspectParams {
                source: source.clone(),
                revision: revision.clone(),
                path: path.clone(),
            })
            .await?;
        if let Some(pin) = &sha256 {
            check_pin(&preview, pin)?;
        }
        if preview.installed.is_some() {
            bail!(
                "a mod {id} is already installed; update it with `imperium mods update {id}`",
                id = preview.id
            );
        }
        print!("{}", render(&preview));
        let answer = approve(&format!("Install {}?", preview.name), yes)?;
        if answer == Answer::No {
            println!("Nothing was installed.");
            return Ok(());
        }
        // Running outside the sandbox is a separate answer, typed at a terminal; `--yes` never
        // gives it.
        let trusted =
            preview.main_context && answer == Answer::Prompted && confirm_trust(&preview)?;
        let request = |trusted: bool| ModInstallParams {
            source: source.clone(),
            revision: revision.clone(),
            path: path.clone(),
            sha256: preview.sha256.clone(),
            permissions: preview.permissions.clone(),
            replace: false,
            interactive: answer == Answer::Prompted,
            trusted,
        };
        let done = match client.call::<ModInstall>(request(trusted)).await {
            // The daemon decides who may grant trust; when it does not count this process, the
            // mod still installs, in its sandbox.
            Err(error)
                if trusted
                    && error
                        .to_string()
                        .contains(imperium_protocol::errors::TRUST_NOT_GRANTED) =>
            {
                println!("Note: {error}\nInstalling it in the sandbox instead.");
                client.call::<ModInstall>(request(false)).await?
            }
            other => other?,
        };
        // What the daemon counted, which is what holds, not what was asked.
        for note in &done.notes {
            println!("Note: {note}");
        }
        if preview.main_context && !done.trusted {
            println!("It asked to run in the app's own window; it runs in the sandbox instead.");
        }
        let standing = if done.interactive {
            "Its panels show in the desktop app."
        } else {
            "It was not counted as approved at a terminal prompt, so it stays off until you turn it on in the desktop app (Settings → Mods)."
        };
        println!(
            "Installed {} {} into {}. {standing}",
            done.id, done.version, done.path
        );
        Ok(())
    })
}

pub fn update(id: String, sha256: Option<String>, yes: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listed = client.call::<ModList>(ModListParams {}).await?;
        let current = listed
            .mods
            .iter()
            .find(|installed| installed.id == id && !installed.bundled)
            .with_context(|| format!("no installed mod {id}"))?;
        let Some(from) = &current.installed_from else {
            bail!(
                "{id} has no recorded source (it was not installed by Imperium); uninstall it \
                 and install it again from its source"
            );
        };
        let preview = client
            .call::<ModInspect>(ModInspectParams {
                source: from.location.clone(),
                revision: from.requested.clone(),
                path: from.subdir.clone(),
            })
            .await?;
        if preview.id != id {
            bail!(
                "{} now holds a different mod ({}), so it was not updated",
                from.location,
                preview.id
            );
        }
        if let Some(pin) = &sha256 {
            check_pin(&preview, pin)?;
        }
        let unchanged = preview
            .installed
            .as_ref()
            .and_then(|installed| installed.sha256.as_deref())
            == Some(preview.sha256.as_str());
        if unchanged {
            println!("{id} {} is up to date.", preview.version);
            return Ok(());
        }
        print!("{}", render(&preview));
        // Asking again is only for more than the user approved before.
        let widens = !preview.added_permissions.is_empty();
        let mut interactive = false;
        if widens {
            let answer = approve(
                &format!("Update {} with these new permissions?", preview.name),
                yes,
            )?;
            if answer == Answer::No {
                println!("Not updated.");
                return Ok(());
            }
            interactive = answer == Answer::Prompted;
        }
        // Trust was for the content the user confirmed: new content asks again.
        let was_trusted = preview.installed.as_ref().is_some_and(|i| i.trusted);
        let trusted = was_trusted && preview.main_context && confirm_trust(&preview)?;
        if was_trusted && !trusted {
            println!("The updated mod runs in the sandbox, not in the app's own window.");
        }
        let request = |trusted: bool| ModInstallParams {
            source: from.location.clone(),
            revision: from.requested.clone(),
            path: from.subdir.clone(),
            sha256: preview.sha256.clone(),
            permissions: preview.permissions.clone(),
            replace: true,
            interactive,
            trusted,
        };
        let done = match client.call::<ModInstall>(request(trusted)).await {
            // When the daemon does not count this process, the update still goes ahead, in the
            // sandbox.
            Err(error)
                if trusted
                    && error
                        .to_string()
                        .contains(imperium_protocol::errors::TRUST_NOT_GRANTED) =>
            {
                println!("Note: {error}\nUpdating it in the sandbox instead.");
                client.call::<ModInstall>(request(false)).await?
            }
            other => other?,
        };
        for note in &done.notes {
            println!("Note: {note}");
        }
        println!(
            "Updated {} to {}{}. The previous folder was kept in the daemon's config-trash.",
            done.id,
            done.version,
            done.replaced
                .map(|old| format!(" (from {old})"))
                .unwrap_or_default()
        );
        Ok(())
    })
}

/// A relative folder, made absolute against this directory (the daemon resolves it on its own
/// machine, which may be elsewhere). Anything else (a URL, or a folder on a remote daemon's
/// machine) is passed on as given.
pub(crate) fn absolute_folder(source: String) -> String {
    let local = crate::REMOTE.get().is_none_or(Option::is_none);
    if local
        && Path::new(&source).is_dir()
        && let Ok(absolute) = std::path::absolute(&source)
    {
        return absolute.display().to_string();
    }
    source
}

fn check_pin(preview: &ModPreview, pin: &str) -> anyhow::Result<()> {
    let pin = pin.trim().trim_start_matches("sha256:");
    if !pin.eq_ignore_ascii_case(&preview.sha256) {
        bail!(
            "the source hashes to {}, not the {pin} you pinned; nothing was installed",
            preview.sha256
        );
    }
    Ok(())
}

/// The warning shown before a mod may run in the app's own window, in plain words, with the
/// content hash the confirmation is for.
fn trust_warning(preview: &ModPreview) -> String {
    format!(
        "{name} ({id}) asks to run INSIDE the app's own window, not in a sandbox. There it can \
         read and change everything the app can: your sessions and settings, and the connection \
         to the daemon (and so the daemon's token), and act as you. Trust it only if you have \
         read its code.\n  This confirmation is for exactly this content: sha256:{hash}\n  \
         A new version asks again. You can turn it off for this mod in Settings → Mods.\n",
        name = preview.name,
        id = preview.id,
        hash = preview.sha256
    )
}

/// Asks the user to type `trust <id>`. Without a terminal, or with any other answer, the mod
/// stays in its sandbox.
fn confirm_trust(preview: &ModPreview) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        println!(
            "{} asks to run in the app's own window; that needs your answer at a terminal, so \
             it stays in the sandbox.",
            preview.id
        );
        return Ok(false);
    }
    print!("{}", trust_warning(preview));
    print!(
        "Type `trust {}` to allow it, or press Enter to keep it sandboxed: ",
        preview.id
    );
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(answer.trim() == format!("trust {}", preview.id))
}

/// How the user answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    No,
    /// By `--yes`.
    Flag,
    /// By typing yes at a prompt in a terminal.
    Prompted,
}

fn approve(question: &str, yes: bool) -> anyhow::Result<Answer> {
    if yes {
        return Ok(Answer::Flag);
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "this needs your approval: run it in a terminal, or pass --yes to approve what is listed above"
        );
    }
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(
        if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            Answer::Prompted
        } else {
            Answer::No
        },
    )
}

/// What the user is asked to approve: the mod, where it is from, its content hash, and its
/// permissions (for an update, marked as new, kept, or no longer requested).
fn render(preview: &ModPreview) -> String {
    let mut text = format!(
        "{} ({}) {}\n  from {}",
        preview.name, preview.id, preview.version, preview.location
    );
    if let Some(revision) = &preview.revision {
        text.push_str(&format!(
            " at {}",
            revision.chars().take(12).collect::<String>()
        ));
    }
    text.push_str(&format!("\n  content sha256:{}\n", preview.sha256));
    if preview.main_context {
        text.push_str("  It asks to run in the app's own window (it runs in a sandbox unless you trust it).\n");
    }
    match &preview.installed {
        Some(installed) => text.push_str(&format!(
            "Update from {} (installed). Requested permissions, against what you approved:\n",
            if installed.version.is_empty() {
                "an unversioned release"
            } else {
                &installed.version
            }
        )),
        None => text.push_str(if preview.permissions.is_empty() {
            "It asks for no permissions.\n"
        } else {
            "It asks for these permissions:\n"
        }),
    }
    let update = preview.installed.is_some();
    // New code under the same permissions is still new code: say so.
    if let Some(old) = preview
        .installed
        .as_ref()
        .and_then(|installed| installed.sha256.as_deref())
        .filter(|old| *old != preview.sha256)
    {
        text.push_str(&format!(
            "  The code changed: sha256:{} -> sha256:{}\n",
            old.chars().take(16).collect::<String>(),
            preview.sha256.chars().take(16).collect::<String>()
        ));
    }
    for permission in &preview.permissions {
        let mark = if update && preview.added_permissions.contains(permission) {
            "+ "
        } else {
            "  "
        };
        text.push_str(&format!("  {mark}{permission}: {}\n", describe(permission)));
    }
    for permission in &preview.removed_permissions {
        text.push_str(&format!("  - {permission}: no longer requested\n"));
    }
    if update && preview.added_permissions.is_empty() && preview.removed_permissions.is_empty() {
        text.push_str("  (the permissions are the same as before)\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use imperium_protocol::methods::ModInstalled;

    use super::*;

    fn preview() -> ModPreview {
        ModPreview {
            id: "demo".into(),
            name: "Demo".into(),
            version: "1.1.0".into(),
            permissions: vec!["storage".into(), "sessions:read".into()],
            panels: Vec::new(),
            main_context: false,
            sha256: "ab".repeat(32),
            location: "https://example.com/demo.git".into(),
            revision: Some("0123456789abcdef".into()),
            installed: None,
            added_permissions: vec!["storage".into(), "sessions:read".into()],
            removed_permissions: Vec::new(),
        }
    }

    #[test]
    fn a_new_mod_lists_what_it_asks_for() {
        let text = render(&preview());
        assert!(text.contains("Demo (demo) 1.1.0"), "{text}");
        assert!(text.contains("at 0123456789ab\n"), "{text}");
        assert!(text.contains("content sha256:abab"), "{text}");
        assert!(text.contains("It asks for these permissions:"), "{text}");
        assert!(text.contains("storage: keep its own settings"), "{text}");
        assert!(text.contains("sessions:read: read your sessions"), "{text}");
        let none = ModPreview {
            permissions: Vec::new(),
            added_permissions: Vec::new(),
            ..preview()
        };
        assert!(render(&none).contains("It asks for no permissions."));
    }

    #[test]
    fn an_update_shows_a_diff_of_the_requested_permissions() {
        let update = ModPreview {
            installed: Some(ModInstalled {
                version: "1.0.0".into(),
                permissions: vec!["storage".into(), "pets:read".into()],
                sha256: None,
                trusted: false,
            }),
            added_permissions: vec!["sessions:read".into()],
            removed_permissions: vec!["pets:read".into()],
            ..preview()
        };
        let text = render(&update);
        assert!(text.contains("Update from 1.0.0"), "{text}");
        assert!(text.contains("    storage:"), "{text}");
        assert!(text.contains("  + sessions:read:"), "{text}");
        assert!(
            text.contains("  - pets:read: no longer requested"),
            "{text}"
        );
        let same = ModPreview {
            added_permissions: Vec::new(),
            removed_permissions: Vec::new(),
            ..update
        };
        assert!(render(&same).contains("the permissions are the same as before"));
    }

    #[test]
    fn an_update_says_when_the_code_changed_even_if_the_permissions_did_not() {
        let update = ModPreview {
            installed: Some(ModInstalled {
                version: "1.0.0".into(),
                permissions: vec!["storage".into(), "sessions:read".into()],
                sha256: Some("cd".repeat(32)),
                trusted: false,
            }),
            added_permissions: Vec::new(),
            removed_permissions: Vec::new(),
            ..preview()
        };
        let text = render(&update);
        assert!(
            text.contains("The code changed: sha256:cdcdcdcdcdcdcdcd -> sha256:abababababababab"),
            "{text}"
        );
        assert!(
            text.contains("the permissions are the same as before"),
            "{text}"
        );
    }

    #[test]
    fn trusting_a_mod_is_a_plain_and_specific_warning() {
        let ask = ModPreview {
            main_context: true,
            ..preview()
        };
        let warning = trust_warning(&ask);
        assert!(warning.contains("INSIDE the app's own window"), "{warning}");
        assert!(warning.contains("not in a sandbox"), "{warning}");
        assert!(warning.contains("sha256:abab"), "{warning}");
        assert!(warning.contains("asks again"), "{warning}");
        assert!(render(&ask).contains("asks to run in the app's own window"));
        assert!(!render(&preview()).contains("own window"));
    }

    #[test]
    fn every_permission_the_daemon_accepts_is_described() {
        for permission in ["telemetry:read", "sessions:read", "pets:read", "storage"] {
            assert_ne!(describe(permission), "(unknown to this version)");
        }
    }

    #[test]
    fn a_pinned_hash_must_match() {
        let preview = preview();
        assert!(check_pin(&preview, &"ab".repeat(32)).is_ok());
        assert!(check_pin(&preview, &format!("sha256:{}", "AB".repeat(32))).is_ok());
        assert!(check_pin(&preview, &"00".repeat(32)).is_err());
    }

    #[test]
    fn approval_without_a_terminal_needs_yes() {
        // `--yes` approves; without it a non-interactive run is refused rather than assumed.
        assert_eq!(approve("Install?", true).unwrap(), Answer::Flag);
        if !std::io::stdin().is_terminal() {
            assert!(approve("Install?", false).is_err());
        }
    }
}
