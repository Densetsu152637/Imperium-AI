//! `imperium daemon autostart`: start `imperium-core` when the user logs in, so schedules, project
//! agents, and auto-fix run without the desktop window. Per-user only: a `Run` registry value
//! on Windows, a LaunchAgent on macOS, an XDG autostart entry on Linux.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

const NAME: &str = "Imperium";
const LABEL: &str = "ai.imperium.daemon";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
/// The XDG autostart file name the daemon had before it was renamed `imperium-core`. Enabling
/// replaces it, disabling removes it, and status still counts it. (The Windows registry value
/// and the macOS LaunchAgent label did not change, so enabling simply rewrites them.)
const LEGACY_DESKTOP_ENTRY: &str = "imperiumd.desktop";

/// `imperium-core` next to the running `imperium` binary.
fn daemon_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("could not find the imperium binary")?;
    let daemon = exe.with_file_name(format!("imperium-core{}", std::env::consts::EXE_SUFFIX));
    if !daemon.is_file() {
        bail!(
            "imperium-core is not next to imperium at {}",
            daemon.display()
        );
    }
    Ok(daemon)
}

/// The command Windows runs at login: starts the daemon without a console window.
pub fn windows_command(daemon: &Path) -> String {
    format!(
        "powershell.exe -NoProfile -WindowStyle Hidden -Command \"Start-Process -WindowStyle Hidden -FilePath '{}'\"",
        daemon.display()
    )
}

/// A LaunchAgent that starts the daemon at login.
pub fn launch_agent(daemon: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string></array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
"#,
        daemon.display()
    )
}

/// An XDG autostart entry that starts the daemon at login.
pub fn desktop_entry(daemon: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={NAME} daemon\nExec=\"{}\"\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
        daemon.display()
    )
}

fn home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn xdg_autostart_dir() -> anyhow::Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map_or_else(|| home().map(|home| home.join(".config")), Ok)?;
    Ok(config.join("autostart"))
}

fn entry_file() -> anyhow::Result<PathBuf> {
    Ok(if cfg!(target_os = "macos") {
        home()?
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    } else {
        xdg_autostart_dir()?.join("imperium-core.desktop")
    })
}

/// The pre-rename autostart entry, on systems that use XDG autostart files.
fn legacy_entry_file() -> anyhow::Result<Option<PathBuf>> {
    if cfg!(windows) || cfg!(target_os = "macos") {
        return Ok(None);
    }
    Ok(Some(xdg_autostart_dir()?.join(LEGACY_DESKTOP_ENTRY)))
}

/// Removes the pre-rename autostart entry; whether there was one.
fn remove_legacy_entry() -> anyhow::Result<bool> {
    let Some(file) = legacy_entry_file()? else {
        return Ok(false);
    };
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("could not remove {}", file.display())),
    }
}

fn reg(args: &[&str]) -> anyhow::Result<std::process::Output> {
    Command::new("reg")
        .args(args)
        .output()
        .context("could not run reg.exe")
}

pub fn enable() -> anyhow::Result<()> {
    let daemon = daemon_path()?;
    if cfg!(windows) {
        let command = windows_command(&daemon);
        let output = reg(&[
            "add", RUN_KEY, "/v", NAME, "/t", "REG_SZ", "/d", &command, "/f",
        ])?;
        if !output.status.success() {
            bail!(
                "reg add failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        println!("imperium-core will start when you log in (registry {RUN_KEY}\\{NAME})");
        return Ok(());
    }
    let file = entry_file()?;
    let contents = if cfg!(target_os = "macos") {
        launch_agent(&daemon)
    } else {
        desktop_entry(&daemon)
    };
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    std::fs::write(&file, contents)
        .with_context(|| format!("could not write {}", file.display()))?;
    remove_legacy_entry()?;
    println!(
        "imperium-core will start when you log in ({})",
        file.display()
    );
    Ok(())
}

pub fn disable() -> anyhow::Result<()> {
    if cfg!(windows) {
        let output = reg(&["delete", RUN_KEY, "/v", NAME, "/f"])?;
        println!(
            "{}",
            if output.status.success() {
                "imperium-core will no longer start at login"
            } else {
                "imperium-core was not set to start at login"
            }
        );
        return Ok(());
    }
    let file = entry_file()?;
    let removed = match std::fs::remove_file(&file) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| format!("could not remove {}", file.display()));
        }
    };
    if remove_legacy_entry()? || removed {
        println!("imperium-core will no longer start at login");
    } else {
        println!("imperium-core was not set to start at login");
    }
    Ok(())
}

pub fn status() -> anyhow::Result<()> {
    let enabled = if cfg!(windows) {
        reg(&["query", RUN_KEY, "/v", NAME])?.status.success()
    } else {
        entry_file()?.is_file() || legacy_entry_file()?.is_some_and(|file| file.is_file())
    };
    println!(
        "start at login: {}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_point_at_the_daemon() {
        let daemon = Path::new("/opt/imperium/imperium-core");
        assert!(launch_agent(daemon).contains("<string>/opt/imperium/imperium-core</string>"));
        assert!(desktop_entry(daemon).contains("Exec=\"/opt/imperium/imperium-core\""));
        let windows = windows_command(Path::new(r"C:\Imperium\imperium-core.exe"));
        assert!(
            windows.contains(r"-FilePath 'C:\Imperium\imperium-core.exe'"),
            "{windows}"
        );
        assert!(windows.contains("-WindowStyle Hidden"));
    }
}
