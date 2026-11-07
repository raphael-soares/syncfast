//! What `syncfast status` prints: one line per sync, from the same
//! [`display_status`] precedence the GUI uses.

use std::fmt;

use jiff::Timestamp;

use crate::config::{Config, GlobalConfigError};
use crate::paths::Paths;
use crate::state::{DisplayStatus, State, display_status, is_running};

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("{0}")]
    Config(#[from] GlobalConfigError),
}

impl StatusError {
    /// 2: a config the user must fix.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub id: String,
    pub status: DisplayStatus,
    pub last_success: Option<Timestamp>,
}

impl fmt::Display for StatusLine {
    /// `<id>\t<status name>\t<last success or "never">`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\t{}\t", self.id, self.status.name())?;
        match self.last_success {
            Some(at) => write!(f, "{at}"),
            None => f.write_str("never"),
        }
    }
}

/// One line per sync in the config, valid or not. A state file that cannot
/// be read shows as that sync's `config_error`.
///
/// # Errors
/// The config has a global problem.
pub async fn collect(paths: &Paths) -> Result<Vec<StatusLine>, StatusError> {
    let config = Config::load(paths).await?;
    let failing_after = config.defaults().notifications.failing_after();
    let mut lines = Vec::with_capacity(config.pairs().len());
    for pair in config.pairs() {
        let state = State::load_or_config_error(paths, &pair.id).await;
        lines.push(StatusLine {
            id: pair.id.clone(),
            status: display_status(pair, &state, is_running(paths, &pair.id), failing_after),
            last_success: state.last_success,
        });
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::state::LastOutcome;

    fn paths(root: &std::path::Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    fn write_config(paths: &Paths, pairs: &[(&str, bool)]) {
        let mut text = String::from("version = 1\n");
        for (id, enabled) in pairs {
            write!(
                text,
                "\n[[pair]]\nid = \"{id}\"\nname = \"{id}\"\nlocal = \"/data/{id}\"\nremote = \"r:{id}\"\nenabled = {enabled}\n"
            )
            .unwrap();
        }
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    #[tokio::test]
    async fn pair_without_state_is_waiting_for_its_first_review_and_never_synced() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, &[("notes", true)]);

        let lines = collect(&paths).await.unwrap();

        let printed: Vec<String> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(printed, ["notes\tNeedsReview\tnever"]);
    }

    #[tokio::test]
    async fn synced_pair_prints_its_status_and_last_success() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, &[("notes", true), ("docs", false)]);
        let at: Timestamp = "2026-11-03T14:00:00Z".parse().unwrap();
        for id in ["notes", "docs"] {
            let state = State {
                gate: None,
                last_outcome: Some(LastOutcome::Ok),
                last_success: Some(at),
                ..State::default()
            };
            state.save(&paths, id).await.unwrap();
        }

        let lines = collect(&paths).await.unwrap();

        let printed: Vec<String> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(
            printed,
            [
                "notes\tUpToDate\t2026-11-03T14:00:00Z",
                "docs\tPaused\t2026-11-03T14:00:00Z"
            ]
        );
    }

    #[tokio::test]
    async fn pair_whose_round_holds_the_lock_is_syncing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, &[("notes", true)]);
        State {
            gate: None,
            ..State::default()
        }
        .save(&paths, "notes")
        .await
        .unwrap();
        let _round = crate::state::try_lock_pair(&paths, "notes")
            .unwrap()
            .unwrap();

        let lines = collect(&paths).await.unwrap();

        assert_eq!(lines[0].status, DisplayStatus::Syncing);
    }

    #[tokio::test]
    async fn one_corrupt_state_file_only_shows_as_an_error_on_its_own_sync() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        write_config(&paths, &[("notes", true), ("docs", true)]);
        State {
            gate: None,
            last_outcome: Some(LastOutcome::Ok),
            ..State::default()
        }
        .save(&paths, "docs")
        .await
        .unwrap();
        let broken = paths.state_file("notes");
        std::fs::create_dir_all(broken.parent().unwrap()).unwrap();
        std::fs::write(broken, "{ not json").unwrap();

        let lines = collect(&paths).await.unwrap();

        let printed: Vec<String> = lines.iter().map(ToString::to_string).collect();
        assert_eq!(printed, ["notes\tError\tnever", "docs\tUpToDate\tnever"]);
    }

    #[tokio::test]
    async fn config_with_a_global_error_fails_with_exit_code_two() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "version = 3\n").unwrap();

        let err = collect(&paths).await.unwrap_err();

        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.to_string(), "unsupported config version 3");
    }
}
