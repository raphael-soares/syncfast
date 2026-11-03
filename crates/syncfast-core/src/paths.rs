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

    /// Where bisync keeps its listings for the pair.
    #[must_use]
    pub fn bisync_workdir(&self, pair_id: &str) -> PathBuf {
        self.state_dir().join("bisync").join(pair_id)
    }

    #[must_use]
    pub fn pair_state_dir(&self, pair_id: &str) -> PathBuf {
        self.state_dir().join("pairs").join(pair_id)
    }

    #[must_use]
    pub fn state_file(&self, pair_id: &str) -> PathBuf {
        self.pair_state_dir(pair_id).join("state.json")
    }

    #[must_use]
    pub fn confirm_file(&self, pair_id: &str) -> PathBuf {
        self.pair_state_dir(pair_id).join("confirm.json")
    }

    #[must_use]
    pub fn runs_dir(&self, pair_id: &str) -> PathBuf {
        self.pair_state_dir(pair_id).join("runs")
    }

    #[must_use]
    pub fn versions_dir(&self, pair_id: &str) -> PathBuf {
        self.data_dir().join("versions").join(pair_id)
    }

    /// Held exclusively while a round runs; see [`crate::state::is_running`].
    #[must_use]
    pub fn lock_file(&self, pair_id: &str) -> PathBuf {
        self.runtime_subdir().join(format!("{pair_id}.lock"))
    }

    /// Held by `syncfast apply` so two applies never interleave.
    #[must_use]
    pub fn apply_lock_file(&self) -> PathBuf {
        self.runtime_subdir().join("apply.lock")
    }

    /// Where systemd looks for the user's units.
    #[must_use]
    pub fn systemd_user_dir(&self) -> PathBuf {
        self.config_home.join("systemd").join("user")
    }

    /// Where `apply` writes the units when systemd is off (dev mode, tests).
    #[must_use]
    pub fn units_preview_dir(&self) -> PathBuf {
        self.state_dir().join("units-preview")
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
    fn per_pair_locations_live_under_their_xdg_roots() {
        let paths = Paths::new(
            "/home/ada".into(),
            "/c".into(),
            "/s".into(),
            "/d".into(),
            "/r".into(),
        );

        assert_eq!(paths.bisync_workdir("p"), Path::new("/s/syncfast/bisync/p"));
        assert_eq!(
            paths.state_file("p"),
            Path::new("/s/syncfast/pairs/p/state.json")
        );
        assert_eq!(
            paths.confirm_file("p"),
            Path::new("/s/syncfast/pairs/p/confirm.json")
        );
        assert_eq!(paths.runs_dir("p"), Path::new("/s/syncfast/pairs/p/runs"));
        assert_eq!(paths.versions_dir("p"), Path::new("/d/syncfast/versions/p"));
        assert_eq!(paths.lock_file("p"), Path::new("/r/syncfast/p.lock"));
        assert_eq!(paths.apply_lock_file(), Path::new("/r/syncfast/apply.lock"));
        assert_eq!(paths.systemd_user_dir(), Path::new("/c/systemd/user"));
        assert_eq!(
            paths.units_preview_dir(),
            Path::new("/s/syncfast/units-preview")
        );
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
