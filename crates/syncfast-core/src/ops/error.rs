use crate::backup::restore_job::RestoreError;
use crate::config::{
    ConfigEditError, ConfigSaveError, ConfigUpdateError, GlobalConfigError, SettingError,
};
use crate::preview::PreviewError;
use crate::rclone::RcloneError;
use crate::run::RunError;
use crate::state::StateError;

/// Why an operation did not happen. The message is what the user or the
/// agent reads; [`OpError::code`] and [`OpError::exit_code`] are what a
/// program branches on.
#[derive(Debug, thiserror::Error)]
pub enum OpError {
    /// The request or the config is wrong; fix it and try again.
    #[error("{0}")]
    Invalid(String),
    #[error("no sync has the id \"{0}\"")]
    NotFound(String),
    #[error("no backup has the id \"{0}\"")]
    NoBackup(String),
    /// A round of the sync is running.
    #[error("this sync is running; try again when it finishes")]
    Busy,
    /// The review a confirmation was asked for deletes files, which only
    /// the user can confirm.
    #[error("This review deletes files. Confirm it in syncfast.")]
    DeletesNeedUser,
    /// The change lowers a safety setting, which only the user can do.
    /// Holds the keys, comma separated.
    #[error("This setting lowers safety ({0}). Change it in syncfast.")]
    NeedsUser(String),
    /// Only the user can do it, whatever the arguments are. Holds the whole
    /// message.
    #[error("{0}")]
    UserOnly(String),
    /// There is no review shown to the agent that matches the current
    /// state of the sync.
    #[error("{0}")]
    ReviewNotShown(String),
    #[error("{0}")]
    Failed(String),
}

impl OpError {
    /// 2 for something the caller must fix, 3 when a round is running, 4
    /// when only the user can confirm, 5 when the review must be shown
    /// again, 1 for anything else.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Invalid(_) | Self::NotFound(_) | Self::NoBackup(_) => 2,
            Self::Busy => 3,
            Self::DeletesNeedUser | Self::NeedsUser(_) | Self::UserOnly(_) => 4,
            Self::ReviewNotShown(_) => 5,
            Self::Failed(_) => 1,
        }
    }

    /// A stable name for the error, for JSON output.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid",
            Self::NotFound(_) | Self::NoBackup(_) => "not_found",
            Self::Busy => "busy",
            Self::DeletesNeedUser => "deletes_need_user",
            Self::NeedsUser(_) | Self::UserOnly(_) => "needs_user",
            Self::ReviewNotShown(_) => "review_not_shown",
            Self::Failed(_) => "failed",
        }
    }
}

impl From<GlobalConfigError> for OpError {
    fn from(err: GlobalConfigError) -> Self {
        Self::Invalid(err.to_string())
    }
}

impl From<SettingError> for OpError {
    fn from(err: SettingError) -> Self {
        match err {
            SettingError::UnknownSync(id) => Self::NotFound(id),
            other => Self::Invalid(other.to_string()),
        }
    }
}

impl From<ConfigSaveError> for OpError {
    fn from(err: ConfigSaveError) -> Self {
        Self::Failed(err.to_string())
    }
}

impl From<ConfigUpdateError> for OpError {
    fn from(err: ConfigUpdateError) -> Self {
        match err {
            ConfigUpdateError::Load(err) => err.into(),
            ConfigUpdateError::Save(err) => err.into(),
        }
    }
}

impl From<ConfigEditError> for OpError {
    fn from(err: ConfigEditError) -> Self {
        Self::Invalid(err.to_string())
    }
}

impl From<RcloneError> for OpError {
    fn from(err: RcloneError) -> Self {
        Self::Failed(err.to_string())
    }
}

impl From<StateError> for OpError {
    fn from(err: StateError) -> Self {
        Self::Failed(err.to_string())
    }
}

impl From<RunError> for OpError {
    fn from(err: RunError) -> Self {
        match err {
            RunError::Config(err) => err.into(),
            RunError::UnknownPair(id) => Self::NotFound(id),
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<PreviewError> for OpError {
    fn from(err: PreviewError) -> Self {
        match err {
            PreviewError::Busy => Self::Busy,
            PreviewError::InvalidPair(message) => Self::Invalid(message),
            PreviewError::Failed { message, .. } => Self::Failed(message),
            PreviewError::Run(err) => err.into(),
        }
    }
}

impl From<RestoreError> for OpError {
    fn from(err: RestoreError) -> Self {
        match err {
            RestoreError::Invalid(message) => Self::Invalid(message),
            RestoreError::NotFound(_) => Self::Invalid(err.to_string()),
            RestoreError::Busy => Self::Busy,
            RestoreError::Failed(message) => Self::Failed(message),
        }
    }
}
