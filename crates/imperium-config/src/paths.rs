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

    /// Written by a running daemon so local clients can find it.
    pub fn daemon_endpoint_file(&self) -> PathBuf {
        self.data_dir.join("daemon.json")
    }

    /// Held locked by the running daemon so only one runs per user.
    pub fn daemon_lock_file(&self) -> PathBuf {
        self.data_dir.join("daemon.lock")
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
}
