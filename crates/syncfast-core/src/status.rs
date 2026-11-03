//! What `syncfast status` prints: one line per sync, from the same
//! [`display_status`] precedence the GUI uses.

use std::fmt;

use jiff::Timestamp;

use crate::config::{Config, GlobalConfigError};
use crate::paths::Paths;
use crate::state::{DisplayStatus, State, StateError, display_status, is_running};

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("{0}")]
    Config(#[from] GlobalConfigError),
    #[error("{0}")]
    State(#[from] StateError),
}

impl StatusError {
    /// 2 for a config the user must fix, 1 for anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) => 2,
            Self::State(_) => 1,
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

/// One line per sync in the config, valid or not.
///
/// # Errors
/// The config has a global problem or a sync's state cannot be read.
pub async fn collect(paths: &Paths) -> Result<Vec<StatusLine>, StatusError> {
    let config = Config::load(paths).await?;
    let mut lines = Vec::with_capacity(config.pairs().len());
    for pair in config.pairs() {
        let state = State::load(paths, &pair.id).await?;
        lines.push(StatusLine {
            id: pair.id.clone(),
            status: display_status(pair, &state, is_running(paths, &pair.id)),
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
    async fn config_with_a_global_error_fails_with_exit_code_two() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "version = 2\n").unwrap();

        let err = collect(&paths).await.unwrap_err();

        assert_eq!(err.exit_code(), 2);
        assert_eq!(err.to_string(), "unsupported config version 2");
    }
}
