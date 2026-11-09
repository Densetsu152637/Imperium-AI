//! Keeping a `llama-server` Imperium did not install up to date. llama.cpp has no updater of
//! its own, so what can be done depends on how it was installed ([`install_kind`]): a package
//! manager's install is upgraded with that package manager, for that package only
//! ([`upgrade_command`]); a checkout or an unpacked folder is never touched, and Imperium's own
//! build is offered instead ([`external_update`]).

use std::path::Path;

/// How a `llama-server` came to be on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// Imperium's own build, under its data folder.
    Managed,
    /// `winget install ggml.llamacpp`.
    Winget,
    /// A scoop app, by its name (`llama.cpp`).
    Scoop(String),
    /// A Chocolatey package.
    Chocolatey,
    /// Homebrew (`brew install llama.cpp`), on macOS or Linux.
    Homebrew,
    /// One of LM Studio's bundled llama.cpp backends; LM Studio updates them itself.
    LmStudio,
    /// A llama.cpp checkout or an unpacked release archive.
    Folder,
    /// Anywhere else (on `PATH`, a system folder).
    Other,
}

impl InstallKind {
    /// A stable name for the protocol: `managed`, `winget`, `scoop`, `chocolatey`, `homebrew`,
    /// `lmstudio`, `folder`, or `other`.
    pub fn name(&self) -> &'static str {
        match self {
            InstallKind::Managed => "managed",
            InstallKind::Winget => "winget",
            InstallKind::Scoop(_) => "scoop",
            InstallKind::Chocolatey => "chocolatey",
            InstallKind::Homebrew => "homebrew",
            InstallKind::LmStudio => "lmstudio",
            InstallKind::Folder => "folder",
            InstallKind::Other => "other",
        }
    }

    /// Whether a package manager owns it.
    pub fn package_manager(&self) -> bool {
        matches!(
            self,
            InstallKind::Winget
                | InstallKind::Scoop(_)
                | InstallKind::Chocolatey
                | InstallKind::Homebrew
        )
    }
}

/// How `server` was installed, read from its path (resolve symbolic links first, so a Homebrew
/// link in `/usr/local/bin` reads as its `Cellar` path). `managed_root` is where Imperium
/// installs its builds.
pub fn install_kind(server: &Path, managed_root: &Path) -> InstallKind {
    if server.starts_with(managed_root) {
        return InstallKind::Managed;
    }
    let parts: Vec<String> = server
        .components()
        .map(|part| part.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let has = |name: &str| parts.iter().any(|part| part == name);
    let after = |name: &str| {
        parts
            .iter()
            .position(|part| part == name)
            .and_then(|at| parts.get(at + 1))
    };
    if has("winget") {
        return InstallKind::Winget;
    }
    if has("scoop") {
        if let Some(app) = after("apps") {
            return InstallKind::Scoop(app.clone());
        }
        return InstallKind::Scoop("llama.cpp".into());
    }
    if has("chocolatey") {
        return InstallKind::Chocolatey;
    }
    if has("cellar") || has("homebrew") || has(".linuxbrew") {
        return InstallKind::Homebrew;
    }
    if has(".lmstudio") {
        return InstallKind::LmStudio;
    }
    if parts
        .iter()
        .any(|part| part.starts_with("llama.cpp") || part.starts_with("llama-b"))
    {
        return InstallKind::Folder;
    }
    InstallKind::Other
}

/// The command that upgrades `kind`'s llama.cpp package and nothing else; `None` for an install
/// no package manager owns. `windows` picks the scoop launcher Windows can start.
pub fn upgrade_command(kind: &InstallKind, windows: bool) -> Option<Vec<String>> {
    let words: Vec<&str> = match kind {
        InstallKind::Winget => vec![
            "winget",
            "upgrade",
            "--id",
            "ggml.llamacpp",
            "--exact",
            "--disable-interactivity",
        ],
        InstallKind::Scoop(app) => {
            let scoop = if windows { "scoop.cmd" } else { "scoop" };
            return Some(vec![scoop.into(), "update".into(), app.clone()]);
        }
        InstallKind::Chocolatey => vec!["choco", "upgrade", "llama.cpp", "-y"],
        InstallKind::Homebrew => vec!["brew", "upgrade", "llama.cpp"],
        _ => return None,
    };
    Some(words.into_iter().map(String::from).collect())
}

/// What to do about a `llama-server` Imperium did not install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalUpdate {
    /// Nothing: it is current, its version is unknown, or updating it is off.
    None,
    /// Offer to run this package-manager upgrade (`update_external = "ask"`).
    Offer(Vec<String>),
    /// Run this upgrade while no model is loaded from it (`update_external = "auto"`).
    Auto(Vec<String>),
    /// It cannot be updated from here (a checkout, an unpacked folder, LM Studio's): offer to
    /// install Imperium's build and use that instead.
    UseManaged,
}

/// What to do about a `llama-server` of `kind` at llama.cpp build `build` (from its
/// `--version`, `None` when unknown) given the pinned build `pinned`; `flags_ok` says whether
/// its `--help` lists every flag Imperium passes. `mode` is `runtime.llamacpp.update_external`
/// (`off`, `ask`, `auto`), which governs package-manager upgrades only.
pub fn external_update(
    kind: &InstallKind,
    build: Option<u64>,
    pinned: Option<u64>,
    flags_ok: bool,
    mode: &str,
    windows: bool,
) -> ExternalUpdate {
    let older = matches!((build, pinned), (Some(build), Some(pinned)) if build < pinned);
    if *kind == InstallKind::Managed || !(older || !flags_ok) {
        return ExternalUpdate::None;
    }
    if let Some(command) = upgrade_command(kind, windows) {
        return match mode {
            "auto" => ExternalUpdate::Auto(command),
            "ask" => ExternalUpdate::Offer(command),
            _ => ExternalUpdate::None,
        };
    }
    ExternalUpdate::UseManaged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(path: &str) -> InstallKind {
        install_kind(Path::new(path), Path::new("/data/runtimes/llama.cpp"))
    }

    #[test]
    fn installs_are_told_apart_by_their_paths() {
        assert_eq!(
            kind("/data/runtimes/llama.cpp/b12180/x/bin/llama-server"),
            InstallKind::Managed
        );
        assert_eq!(
            kind(
                "C:/Users/me/AppData/Local/Microsoft/WinGet/Packages/ggml.llamacpp_x/llama-server.exe"
            ),
            InstallKind::Winget
        );
        assert_eq!(
            kind("C:/Users/me/AppData/Local/Microsoft/WinGet/Links/llama-server.exe"),
            InstallKind::Winget
        );
        assert_eq!(
            kind("C:/Users/me/scoop/apps/llama.cpp-cuda/current/llama-server.exe"),
            InstallKind::Scoop("llama.cpp-cuda".into())
        );
        assert_eq!(
            kind("C:/Users/me/scoop/shims/llama-server.exe"),
            InstallKind::Scoop("llama.cpp".into())
        );
        assert_eq!(
            kind("C:/ProgramData/chocolatey/bin/llama-server.exe"),
            InstallKind::Chocolatey
        );
        assert_eq!(
            kind("/opt/homebrew/Cellar/llama.cpp/6000/bin/llama-server"),
            InstallKind::Homebrew
        );
        assert_eq!(
            kind("/home/linuxbrew/.linuxbrew/bin/llama-server"),
            InstallKind::Homebrew
        );
        assert_eq!(
            kind(
                "/home/me/.lmstudio/extensions/backends/llama.cpp-linux-x86_64-2.1.0/llama-server"
            ),
            InstallKind::LmStudio
        );
        assert_eq!(
            kind("/home/me/.unsloth/llama.cpp/build/bin/llama-server"),
            InstallKind::Folder
        );
        assert_eq!(
            kind("/home/me/Downloads/llama-b9000-bin-ubuntu-x64/bin/llama-server"),
            InstallKind::Folder
        );
        assert_eq!(kind("/usr/bin/llama-server"), InstallKind::Other);
    }

    #[test]
    fn upgrades_name_only_the_llama_cpp_package() {
        assert_eq!(
            upgrade_command(&InstallKind::Winget, true).unwrap()[..4],
            ["winget", "upgrade", "--id", "ggml.llamacpp"]
        );
        assert_eq!(
            upgrade_command(&InstallKind::Scoop("llama.cpp".into()), true).unwrap(),
            ["scoop.cmd", "update", "llama.cpp"]
        );
        assert_eq!(
            upgrade_command(&InstallKind::Homebrew, false).unwrap(),
            ["brew", "upgrade", "llama.cpp"]
        );
        assert_eq!(
            upgrade_command(&InstallKind::Chocolatey, true).unwrap()[..3],
            ["choco", "upgrade", "llama.cpp"]
        );
        assert_eq!(upgrade_command(&InstallKind::Folder, true), None);
        assert_eq!(upgrade_command(&InstallKind::LmStudio, true), None);
    }

    #[test]
    fn package_manager_installs_follow_the_update_setting() {
        let winget = InstallKind::Winget;
        let old = Some(9000);
        let pinned = Some(12180);
        assert!(matches!(
            external_update(&winget, old, pinned, true, "ask", true),
            ExternalUpdate::Offer(_)
        ));
        assert!(matches!(
            external_update(&winget, old, pinned, true, "auto", true),
            ExternalUpdate::Auto(_)
        ));
        assert_eq!(
            external_update(&winget, old, pinned, true, "off", true),
            ExternalUpdate::None
        );
        assert_eq!(
            external_update(&winget, Some(12180), pinned, true, "auto", true),
            ExternalUpdate::None,
            "current"
        );
        assert_eq!(
            external_update(&winget, None, pinned, true, "auto", true),
            ExternalUpdate::None,
            "unknown version"
        );
        assert!(matches!(
            external_update(&winget, None, pinned, false, "ask", true),
            ExternalUpdate::Offer(_)
        ));
    }

    #[test]
    fn folders_are_never_modified_and_offer_imperiums_build() {
        for kind in [
            InstallKind::Folder,
            InstallKind::LmStudio,
            InstallKind::Other,
        ] {
            assert_eq!(
                external_update(&kind, Some(9000), Some(12180), true, "auto", false),
                ExternalUpdate::UseManaged
            );
            assert_eq!(
                external_update(&kind, None, Some(12180), false, "off", false),
                ExternalUpdate::UseManaged
            );
            assert_eq!(
                external_update(&kind, Some(12180), Some(12180), true, "auto", false),
                ExternalUpdate::None
            );
        }
        assert_eq!(
            external_update(
                &InstallKind::Managed,
                Some(1),
                Some(12180),
                false,
                "auto",
                false
            ),
            ExternalUpdate::None
        );
    }
}
