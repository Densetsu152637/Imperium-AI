use std::path::{Path, PathBuf};

/// Where Imperium keeps configuration, data, and caches for the current user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("could not determine the home directory; set IMPERIUM_HOME")]
    NoHomeDirectory,
}

impl Paths {
    /// Uses `IMPERIUM_HOME` when set and non-empty, otherwise the platform's conventional
    /// per-user directories.
    pub fn from_env() -> Result<Self, PathsError> {
        match std::env::var_os("IMPERIUM_HOME") {
            Some(home) if !home.is_empty() => Ok(Self::under(Path::new(&home))),
            _ => Self::platform(),
        }
    }

    /// Config, data and cache in the one folder `dir`, as on macOS, where the platform gives all
    /// three the same folder. For tests of what a shared folder must not guard.
    pub fn in_one_folder(dir: &Path) -> Self {
        Paths {
            config_dir: dir.to_path_buf(),
            data_dir: dir.to_path_buf(),
            cache_dir: dir.to_path_buf(),
        }
    }

    /// All locations as subdirectories of one root.
    pub fn under(home: &Path) -> Self {
        Paths {
            config_dir: home.join("config"),
            data_dir: home.join("data"),
            cache_dir: home.join("cache"),
        }
    }

    fn platform() -> Result<Self, PathsError> {
        let dirs = directories::ProjectDirs::from("", "", "Imperium")
            .ok_or(PathsError::NoHomeDirectory)?;
        Ok(Paths {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_local_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// The user configuration layer.
    pub fn user_config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// The layer the desktop app writes model settings to; it wins over `config.toml`.
    pub fn app_models_file(&self) -> PathBuf {
        self.config_dir.join("models.toml")
    }

    /// Written by a running daemon so local clients can find it.
    pub fn daemon_endpoint_file(&self) -> PathBuf {
        self.data_dir.join("daemon.json")
    }

    /// Held locked by the running daemon so only one runs per user.
    pub fn daemon_lock_file(&self) -> PathBuf {
        self.data_dir.join("daemon.lock")
    }

    /// The local socket's folder (`imperium-ipc` binds `daemon.sock` inside it).
    pub fn daemon_socket_dir(&self) -> PathBuf {
        self.data_dir.join("ipc")
    }

    /// The profile of the browser agents drive (`<data>/browser`): it holds whatever the user
    /// signed in to there, cookies included, so it is a secret path (see
    /// [`Self::daemon_credential_paths`]).
    pub fn browser_profile_dir(&self) -> PathBuf {
        self.data_dir.join("browser")
    }

    /// What a command an agent runs must not be able to read or open, because it would hand it
    /// the daemon or the user's sign-ins: the endpoint file (it holds the access token), the
    /// local socket's folder, the paired phones' file, the remote daemons' endpoint files (their
    /// tokens), the encrypted secret store with its key, and the signed-in browser profile. The
    /// OS isolation tier hides them (`imperium_tools::hide_from_os_sandbox`).
    pub fn daemon_credential_paths(&self) -> Vec<PathBuf> {
        vec![
            self.daemon_endpoint_file(),
            self.daemon_socket_dir(),
            self.data_dir.join("devices.json"),
            // Forwarded remote daemons: their tokens and loopback ports (`imperium remote`).
            self.data_dir.join("remotes"),
            self.data_dir.join("secrets.key"),
            self.data_dir.join("secrets.enc"),
            self.data_dir.join("secrets.lock"),
            // The browser's cookies and saved sign-ins.
            self.browser_profile_dir(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_root_holds_every_location() {
        let paths = Paths::under(Path::new("/opt/imperium"));
        assert_eq!(paths.config_dir(), Path::new("/opt/imperium/config"));
        assert_eq!(
            paths.user_config_file(),
            Path::new("/opt/imperium/config/config.toml")
        );
        assert_eq!(
            paths.daemon_endpoint_file(),
            Path::new("/opt/imperium/data/daemon.json")
        );
        assert_eq!(paths.cache_dir(), Path::new("/opt/imperium/cache"));
    }

    #[test]
    fn the_credential_paths_name_the_token_the_socket_and_the_secret_store() {
        let paths = Paths::under(Path::new("/opt/imperium"));
        let credentials = paths.daemon_credential_paths();
        for expected in [
            "/opt/imperium/data/daemon.json",
            "/opt/imperium/data/ipc",
            "/opt/imperium/data/devices.json",
            "/opt/imperium/data/remotes",
            "/opt/imperium/data/secrets.key",
            "/opt/imperium/data/secrets.enc",
            "/opt/imperium/data/browser",
        ] {
            assert!(
                credentials.contains(&PathBuf::from(expected)),
                "{expected} missing from {credentials:?}"
            );
        }
    }
}
