//! The single resolver for every directory syncfast reads or writes.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

const APP_DIR: &str = "syncfast";

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("HOME is not set to an absolute path")]
    HomeNotSet,
    #[error("cannot determine the user id for the runtime directory: {0}")]
    Uid(#[source] std::io::Error),
}

/// XDG base directories plus `HOME` (for `~` expansion). Built by hand in
/// tests and in dev mode so nothing depends on process-wide variables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub state_home: PathBuf,
    pub data_home: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Paths {
    #[must_use]
    pub fn new(
        home: PathBuf,
        config_home: PathBuf,
        state_home: PathBuf,
        data_home: PathBuf,
        runtime_dir: PathBuf,
    ) -> Self {
        Self {
            home,
            config_home,
            state_home,
            data_home,
            runtime_dir,
        }
    }

    /// Reads `HOME` and the `XDG_*` variables, with the XDG fallbacks for the
    /// ones that are unset, empty or relative.
    ///
    /// # Errors
    /// `HOME` is unset or relative, or the runtime directory needs the
    /// `/run/user/<uid>` fallback and the uid cannot be read.
    pub fn from_env() -> Result<Self, PathsError> {
        Self::from_lookup(|key| std::env::var_os(key), current_uid)
    }

    pub(crate) fn from_lookup(
        lookup: impl Fn(&str) -> Option<OsString>,
        uid: impl FnOnce() -> std::io::Result<u32>,
    ) -> Result<Self, PathsError> {
        let absolute = |key: &str| {
            lookup(key)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let home = absolute("HOME").ok_or(PathsError::HomeNotSet)?;
        let runtime_dir = match absolute("XDG_RUNTIME_DIR") {
            Some(dir) => dir,
            None => PathBuf::from(format!("/run/user/{}", uid().map_err(PathsError::Uid)?)),
        };
        Ok(Self {
            config_home: absolute("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")),
            state_home: absolute("XDG_STATE_HOME").unwrap_or_else(|| home.join(".local/state")),
            data_home: absolute("XDG_DATA_HOME").unwrap_or_else(|| home.join(".local/share")),
            runtime_dir,
            home,
        })
    }

    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        self.config_home.join(APP_DIR)
    }

    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.state_home.join(APP_DIR)
    }

    /// Holds the per-pair versions folders; no synced folder may contain it.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.data_home.join(APP_DIR)
    }

    #[must_use]
    pub fn runtime_subdir(&self) -> PathBuf {
        self.runtime_dir.join(APP_DIR)
    }

    #[must_use]
    pub fn filters_file(&self, pair_id: &str) -> PathBuf {
        self.state_dir()
            .join("filters")
            .join(format!("{pair_id}.txt"))
    }
}

fn current_uid() -> std::io::Result<u32> {
    Ok(std::fs::metadata(Path::new("/proc/self"))?.uid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn unset_xdg_variables_fall_back_to_home_based_defaults() {
        let paths = Paths::from_lookup(lookup(&[("HOME", "/home/ada")]), || Ok(1000)).unwrap();

        assert_eq!(paths.config_home, Path::new("/home/ada/.config"));
        assert_eq!(paths.state_home, Path::new("/home/ada/.local/state"));
        assert_eq!(paths.data_home, Path::new("/home/ada/.local/share"));
        assert_eq!(paths.runtime_dir, Path::new("/run/user/1000"));
    }

    #[test]
    fn set_xdg_variables_override_the_fallbacks() {
        let vars = [
            ("HOME", "/home/ada"),
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_STATE_HOME", "/s"),
            ("XDG_DATA_HOME", "/d"),
            ("XDG_RUNTIME_DIR", "/r"),
        ];

        let paths = Paths::from_lookup(lookup(&vars), unreachable_uid).unwrap();

        assert_eq!(paths.config_file(), Path::new("/c/syncfast/config.toml"));
        assert_eq!(paths.state_dir(), Path::new("/s/syncfast"));
        assert_eq!(paths.data_dir(), Path::new("/d/syncfast"));
        assert_eq!(paths.runtime_subdir(), Path::new("/r/syncfast"));
        assert_eq!(
            paths.filters_file("drive"),
            Path::new("/s/syncfast/filters/drive.txt")
        );
    }

    #[test]
    fn empty_and_relative_xdg_values_are_ignored() {
        let vars = [
            ("HOME", "/home/ada"),
            ("XDG_CONFIG_HOME", ""),
            ("XDG_STATE_HOME", "relative/state"),
        ];

        let paths = Paths::from_lookup(lookup(&vars), || Ok(7)).unwrap();

        assert_eq!(paths.config_home, Path::new("/home/ada/.config"));
        assert_eq!(paths.state_home, Path::new("/home/ada/.local/state"));
    }

    #[test]
    fn missing_home_is_an_error() {
        let result = Paths::from_lookup(lookup(&[("XDG_RUNTIME_DIR", "/r")]), || Ok(1));

        assert!(matches!(result, Err(PathsError::HomeNotSet)));
    }

    fn unreachable_uid() -> std::io::Result<u32> {
        Err(std::io::Error::other("uid must not be read"))
    }
}
