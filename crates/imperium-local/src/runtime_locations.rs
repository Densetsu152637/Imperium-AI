//! Where `llama-server` builds are usually left on each OS: `PATH`, package managers (winget,
//! scoop, Chocolatey, Homebrew), LM Studio's bundled llama.cpp backends, and llama.cpp checkouts
//! in fixed, conventional folders. Checkouts and unpacked release archives in the home and
//! Downloads folders ([`unverified_places`]) are only ever *listed*, never run or chosen
//! automatically: an unverified download becomes the runtime only when the user adopts it.
//!
//! [`places`] is pure: it is given the OS, the home folder and the environment, and lists the
//! folders to look in, some of them as "every child of this folder whose name starts with ..."
//! ([`Place::Children`]). [`expand`] reads those parents, one level deep and a bounded number of
//! entries each, so no disk is scanned recursively.

use std::cmp::Ordering;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Children read from one [`Place::Children`] parent at most, newest first.
const MAX_CHILDREN: usize = 8;

/// The operating system the locations are for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

impl Os {
    /// The OS this build runs on.
    pub fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }

    /// `llama-server` with this OS's executable suffix.
    pub fn server_name(self) -> &'static str {
        match self {
            Os::Windows => "llama-server.exe",
            Os::MacOs | Os::Linux => "llama-server",
        }
    }
}

/// A folder `llama-server` may be in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    /// `llama-server` directly in this folder.
    Dir(PathBuf),
    /// Every child folder of `parent` whose name starts with `prefix` (ignoring case), newest
    /// first; in each, `llama-server` is looked for in each of `subdirs` (`""` for the child
    /// itself).
    Children {
        parent: PathBuf,
        prefix: &'static str,
        subdirs: &'static [&'static str],
    },
}

/// Where a llama.cpp checkout keeps `llama-server` once built, and where a release archive has
/// it, best first.
const CHECKOUT: &[&str] = &["build/bin/Release", "build/bin", "bin", ""];

/// The places to look for `llama-server` on `os`, in order: `PATH`, then the folders llama.cpp
/// is usually installed in or unpacked to. `var` reads an environment variable.
pub fn places(os: Os, home: Option<&Path>, var: &dyn Fn(&str) -> Option<OsString>) -> Vec<Place> {
    let mut places = Vec::new();
    if let Some(paths) = var("PATH") {
        places.extend(std::env::split_paths(&paths).map(Place::Dir));
    }
    let checkout = |dir: PathBuf| CHECKOUT.iter().map(move |sub| Place::Dir(dir.join(sub)));
    if let Some(home) = home {
        places.extend(checkout(home.join(".unsloth").join("llama.cpp")));
        places.extend(checkout(home.join("llama.cpp")));
    }
    match os {
        Os::Windows => {
            let var_dir = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
            if let Some(local) = var_dir("LOCALAPPDATA") {
                let winget = local.join("Microsoft").join("WinGet");
                places.push(Place::Dir(winget.join("Links")));
                places.push(Place::Children {
                    parent: winget.join("Packages"),
                    prefix: "ggml.llamacpp",
                    subdirs: &["", "bin"],
                });
                places.extend(checkout(local.join("llama.cpp")));
            }
            let scoop_roots = [
                var_dir("SCOOP").or_else(|| home.map(|home| home.join("scoop"))),
                var_dir("SCOOP_GLOBAL")
                    .or_else(|| var_dir("ProgramData").map(|data| data.join("scoop"))),
            ];
            for root in scoop_roots.into_iter().flatten() {
                // The app folder holds the real build (with its DLLs); the shim only runs it.
                places.push(Place::Children {
                    parent: root.join("apps"),
                    prefix: "llama",
                    subdirs: &["current", "current/bin"],
                });
                places.push(Place::Dir(root.join("shims")));
            }
            let chocolatey = var_dir("ChocolateyInstall")
                .or_else(|| var_dir("ProgramData").map(|data| data.join("chocolatey")));
            if let Some(chocolatey) = chocolatey {
                places.push(Place::Dir(chocolatey.join("bin")));
            }
        }
        Os::MacOs => {
            places.push(Place::Dir("/opt/homebrew/bin".into()));
            places.push(Place::Dir("/usr/local/bin".into()));
            if let Some(home) = home {
                places.push(Place::Dir(home.join(".local").join("bin")));
            }
        }
        Os::Linux => {
            places.push(Place::Dir("/usr/local/bin".into()));
            places.push(Place::Dir("/usr/bin".into()));
            places.push(Place::Dir("/home/linuxbrew/.linuxbrew/bin".into()));
            if let Some(home) = home {
                places.push(Place::Dir(home.join(".local").join("bin")));
            }
            places.extend(checkout(PathBuf::from("/opt/llama.cpp")));
        }
    }
    if let Some(home) = home {
        // LM Studio ships a llama-server in each llama.cpp backend it downloads, such as
        // `~/.lmstudio/extensions/backends/llama.cpp-win-x86_64-nvidia-cuda12-avx2-2.21.0/`.
        places.push(Place::Children {
            parent: home.join(".lmstudio").join("extensions").join("backends"),
            prefix: "llama.cpp",
            subdirs: &[""],
        });
    }
    places
}

/// Folders where an unverified `llama.cpp` checkout or unpacked release archive
/// (`llama-b12180-bin-win-cuda-13.5-x64`) is often left: the home and Downloads folders. These
/// are offered to the user as candidates and are never probed or used automatically.
pub fn unverified_places(home: Option<&Path>) -> Vec<Place> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut places = Vec::new();
    for parent in [home.to_path_buf(), home.join("Downloads")] {
        for prefix in ["llama.cpp", "llama-b"] {
            places.push(Place::Children {
                parent: parent.clone(),
                prefix,
                subdirs: CHECKOUT,
            });
        }
    }
    places
}

/// The `llama-server` files at `places` (in order, once each) for an OS whose executable is
/// called `server`. A [`Place::Children`] parent is read one level deep, newest first, and only
/// its first [`MAX_CHILDREN`] matching children are looked in.
pub fn expand(places: &[Place], server: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut add = |path: PathBuf| {
        if path.is_file() && !found.contains(&path) {
            found.push(path);
        }
    };
    for place in places {
        match place {
            Place::Dir(dir) => add(dir.join(server)),
            Place::Children {
                parent,
                prefix,
                subdirs,
            } => {
                for child in children(parent, prefix) {
                    for sub in *subdirs {
                        add(child.join(sub).join(server));
                    }
                }
            }
        }
    }
    found
}

/// The child folders of `parent` whose names start with `prefix` (ignoring case), newest
/// first by the version numbers in their names, at most [`MAX_CHILDREN`].
fn children(parent: &Path, prefix: &str) -> Vec<PathBuf> {
    let prefix = prefix.to_ascii_lowercase();
    let mut dirs: Vec<(String, PathBuf)> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            (name.to_ascii_lowercase().starts_with(&prefix) && path.is_dir())
                .then_some((name, path))
        })
        .collect();
    dirs.sort_by(|(a, _), (b, _)| newest_first(a, b));
    dirs.into_iter()
        .take(MAX_CHILDREN)
        .map(|(_, path)| path)
        .collect()
}

/// Orders names newest first: by the numbers in them, compared as numbers (`2.71.0` after
/// `2.9.0`, `b12180` after `b9000`), then by name.
pub fn newest_first(a: &str, b: &str) -> Ordering {
    numbers(b).cmp(&numbers(a)).then_with(|| a.cmp(b))
}

fn numbers(name: &str) -> Vec<u64> {
    name.split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().unwrap_or(u64::MAX))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let vars: HashMap<String, OsString> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), OsString::from(value)))
            .collect();
        move |name| vars.get(name).cloned()
    }

    fn dirs(places: &[Place]) -> Vec<PathBuf> {
        places
            .iter()
            .filter_map(|place| match place {
                Place::Dir(dir) => Some(dir.clone()),
                Place::Children { .. } => None,
            })
            .collect()
    }

    fn parents(places: &[Place]) -> Vec<(PathBuf, &'static str)> {
        places
            .iter()
            .filter_map(|place| match place {
                Place::Children { parent, prefix, .. } => Some((parent.clone(), *prefix)),
                Place::Dir(_) => None,
            })
            .collect()
    }

    /// Unverified builds in the home or Downloads folders are never probed automatically.
    fn assert_no_download_globs(parents: &[(PathBuf, &'static str)], home: &Path) {
        for (parent, prefix) in parents {
            assert!(
                parent != home && !parent.starts_with(home.join("Downloads")),
                "automatic probing must not glob {} for {prefix}*",
                parent.display()
            );
        }
    }

    #[test]
    fn unverified_downloads_are_only_in_the_unverified_places() {
        let dir = tempfile::tempdir().unwrap();
        let server = Os::Linux.server_name();
        let download = dir.path().join("Downloads").join("llama-b99999");
        std::fs::create_dir_all(&download).unwrap();
        std::fs::write(download.join(server), "").unwrap();
        let checkout = dir.path().join("llama.cpp-b1").join("build/bin");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join(server), "").unwrap();
        let automatic: Vec<_> = expand(&places(Os::Linux, Some(dir.path()), &env(&[])), server)
            .into_iter()
            .filter(|path| path.starts_with(dir.path()))
            .collect();
        assert!(automatic.is_empty(), "{automatic:?}");
        let listed = expand(&unverified_places(Some(dir.path())), server);
        assert_eq!(listed, [checkout.join(server), download.join(server)]);
        assert!(unverified_places(None).is_empty());
    }

    #[test]
    fn windows_looks_in_winget_scoop_chocolatey_and_lm_studio() {
        let home = PathBuf::from("C:/Users/me");
        let local = PathBuf::from("C:/Users/me/AppData/Local");
        let path = std::env::join_paths([PathBuf::from("tools")]).unwrap();
        let vars = env(&[
            ("PATH", path.to_str().unwrap()),
            ("LOCALAPPDATA", "C:/Users/me/AppData/Local"),
            ("ProgramData", "C:/ProgramData"),
        ]);
        let places = places(Os::Windows, Some(&home), &vars);
        let dirs = dirs(&places);
        assert_eq!(dirs[0], PathBuf::from("tools"), "PATH comes first");
        for expected in [
            local.join("Microsoft").join("WinGet").join("Links"),
            local.join("llama.cpp").join("build/bin/Release"),
            home.join("scoop").join("shims"),
            PathBuf::from("C:/ProgramData").join("scoop").join("shims"),
            PathBuf::from("C:/ProgramData")
                .join("chocolatey")
                .join("bin"),
            home.join(".unsloth")
                .join("llama.cpp")
                .join("build/bin/Release"),
        ] {
            assert!(dirs.contains(&expected), "missing {}", expected.display());
        }
        let parents = parents(&places);
        for expected in [
            (
                local.join("Microsoft").join("WinGet").join("Packages"),
                "ggml.llamacpp",
            ),
            (home.join("scoop").join("apps"), "llama"),
            (
                home.join(".lmstudio").join("extensions").join("backends"),
                "llama.cpp",
            ),
        ] {
            assert!(parents.contains(&expected), "missing {expected:?}");
        }
        assert_no_download_globs(&parents, &home);
        assert!(!dirs.contains(&PathBuf::from("/usr/local/bin")));
    }

    #[test]
    fn a_configured_scoop_and_chocolatey_root_is_used() {
        let vars = env(&[("SCOOP", "D:/scoop"), ("ChocolateyInstall", "D:/choco")]);
        let places = places(Os::Windows, None, &vars);
        let dirs = dirs(&places);
        assert!(dirs.contains(&PathBuf::from("D:/scoop").join("shims")));
        assert!(dirs.contains(&PathBuf::from("D:/choco").join("bin")));
        assert!(parents(&places).contains(&(PathBuf::from("D:/scoop").join("apps"), "llama")));
    }

    #[test]
    fn macos_looks_in_homebrew_and_local_bin() {
        let home = PathBuf::from("/Users/me");
        let places = places(Os::MacOs, Some(&home), &env(&[]));
        let dirs = dirs(&places);
        for expected in [
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            home.join(".local").join("bin"),
            home.join("llama.cpp").join("build/bin"),
        ] {
            assert!(dirs.contains(&expected), "missing {}", expected.display());
        }
        assert!(
            !dirs
                .iter()
                .any(|dir| dir.to_string_lossy().contains("WinGet"))
        );
        assert_no_download_globs(&parents(&places), &home);
    }

    #[test]
    fn linux_looks_in_system_folders_and_opt() {
        let home = PathBuf::from("/home/me");
        let places = places(Os::Linux, Some(&home), &env(&[]));
        let dirs = dirs(&places);
        for expected in [
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
            home.join(".local").join("bin"),
            PathBuf::from("/opt/llama.cpp").join("build/bin"),
            PathBuf::from("/opt/llama.cpp").join(""),
        ] {
            assert!(dirs.contains(&expected), "missing {}", expected.display());
        }
        assert!(!dirs.contains(&PathBuf::from("/opt/homebrew/bin")));
    }

    #[test]
    fn without_a_home_only_system_places_remain() {
        let places = places(Os::Linux, None, &env(&[]));
        assert!(parents(&places).is_empty());
        assert!(dirs(&places).contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn names_sort_newest_first_by_their_numbers() {
        let mut names = vec![
            "llama.cpp-win-x86_64-avx2-2.9.0",
            "llama.cpp-win-x86_64-avx2-2.71.0",
            "llama.cpp-win-x86_64-avx2-2.21.0",
        ];
        names.sort_by(|a, b| newest_first(a, b));
        assert_eq!(
            names,
            [
                "llama.cpp-win-x86_64-avx2-2.71.0",
                "llama.cpp-win-x86_64-avx2-2.21.0",
                "llama.cpp-win-x86_64-avx2-2.9.0",
            ]
        );
        assert_eq!(
            newest_first("llama-b12180-bin", "llama-b9000-bin"),
            Ordering::Less
        );
    }

    #[test]
    fn children_are_read_one_level_deep_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let server = Os::current().server_name();
        let backends = dir.path().join("backends");
        for version in 0..12 {
            let child = backends.join(format!("llama.cpp-cuda-1.{version}.0"));
            std::fs::create_dir_all(&child).unwrap();
            std::fs::write(child.join(server), "").unwrap();
        }
        // Too deep: only the child itself is looked in.
        let deep = backends.join("llama.cpp-deep").join("x").join("y");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join(server), "").unwrap();
        std::fs::create_dir_all(backends.join("mlx-engine")).unwrap();
        std::fs::write(backends.join("mlx-engine").join(server), "").unwrap();
        let places = [
            Place::Children {
                parent: backends.clone(),
                prefix: "LLAMA.CPP",
                subdirs: &[""],
            },
            Place::Dir(backends.join("llama.cpp-cuda-1.11.0")),
        ];
        let found = expand(&places, server);
        assert_eq!(found.len(), MAX_CHILDREN, "{found:?}");
        assert_eq!(
            found[0],
            backends.join("llama.cpp-cuda-1.11.0").join(server),
            "newest first, and listed once"
        );
        assert!(found.iter().all(|path| !path.starts_with(&deep)));
        assert!(
            found
                .iter()
                .all(|path| !path.to_string_lossy().contains("mlx"))
        );
    }
}
