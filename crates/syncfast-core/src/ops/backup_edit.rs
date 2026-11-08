//! Creating and changing a backup for an agent: the steps the GUI takes,
//! except that anything that lowers safety is refused (spec phase 3, step
//! 15).

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{BackupView, OpError, Ops};
use crate::backup::config::{BackupConfig, backup_lowers_safety};
use crate::backup::destination::RepositoryError;
use crate::backup::key::Key;
use crate::backup::lifecycle::{
    CreateError, NewBackup, SaveSettingsError, create_backup, new_backup_id, save_backup_settings,
};
use crate::config::{expand_tilde, lexical_clean};
use crate::notify::notify_backup;

const KEYS: &str = "name, paths, dest, every, keep.daily, keep.weekly, keep.monthly, keep.yearly, presets, filters, limit_upload, limit_download or one_file_system";

/// The `keep` values a request sets; the ones it leaves out stay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeepPartial {
    pub daily: Option<i64>,
    pub weekly: Option<i64>,
    pub monthly: Option<i64>,
    pub yearly: Option<i64>,
}

impl KeepPartial {
    /// `daily=7,weekly=4`.
    ///
    /// # Errors
    /// A pair is not `period=number` or the period does not exist.
    pub fn parse(text: &str) -> Result<Self, OpError> {
        let mut keep = Self::default();
        for pair in text
            .split(',')
            .map(str::trim)
            .filter(|pair| !pair.is_empty())
        {
            let Some((period, value)) = pair.split_once('=') else {
                return Err(OpError::Invalid(format!(
                    "keep takes period=number pairs, such as daily=7; got \"{pair}\""
                )));
            };
            keep.set(
                period.trim(),
                number(&format!("keep.{}", period.trim()), value)?,
            )?;
        }
        Ok(keep)
    }

    fn set(&mut self, period: &str, value: i64) -> Result<(), OpError> {
        let slot = match period {
            "daily" => &mut self.daily,
            "weekly" => &mut self.weekly,
            "monthly" => &mut self.monthly,
            "yearly" => &mut self.yearly,
            other => {
                return Err(OpError::Invalid(format!(
                    "unknown keep period \"{other}\": use daily, weekly, monthly or yearly"
                )));
            }
        };
        *slot = Some(value);
        Ok(())
    }

    fn apply_to(self, backup: &mut BackupConfig) {
        let keep = &mut backup.keep;
        keep.daily = self.daily.unwrap_or(keep.daily);
        keep.weekly = self.weekly.unwrap_or(keep.weekly);
        keep.monthly = self.monthly.unwrap_or(keep.monthly);
        keep.yearly = self.yearly.unwrap_or(keep.yearly);
    }
}

/// What a backup is told to do, apart from where it is and what it backs
/// up. The values are checked when the backup is saved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupOptions {
    pub every: Option<String>,
    pub keep: KeepPartial,
    pub presets: Option<Vec<String>>,
    pub filters: Option<Vec<String>>,
    pub limit_upload: Option<String>,
    pub limit_download: Option<String>,
}

impl BackupOptions {
    fn apply_to(&self, backup: &mut BackupConfig) {
        if let Some(every) = &self.every {
            backup.every.clone_from(every);
        }
        self.keep.apply_to(backup);
        if let Some(presets) = &self.presets {
            backup.presets.clone_from(presets);
        }
        if let Some(filters) = &self.filters {
            backup.filters.clone_from(filters);
        }
        if let Some(limit) = &self.limit_upload {
            backup.limit_upload.clone_from(limit);
        }
        if let Some(limit) = &self.limit_download {
            backup.limit_download.clone_from(limit);
        }
    }
}

/// A change to a backup: every field that is `Some` replaces the stored
/// value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackupEdit {
    pub name: Option<String>,
    /// Folders as the caller wrote them: `~` and relative paths are
    /// resolved when the change is applied.
    pub paths: Option<Vec<String>>,
    pub dest: Option<String>,
    pub one_file_system: Option<bool>,
    pub options: BackupOptions,
}

impl BackupEdit {
    /// The change `syncfast backup set <id> <key> <value>...` asks for.
    ///
    /// # Errors
    /// The key does not exist, or the values are not what it takes.
    pub fn setting(key: &str, values: &[String]) -> Result<Self, OpError> {
        if let Some(flag) = values.iter().find(|value| value.starts_with("--")) {
            return Err(OpError::Invalid(format!(
                "\"{flag}\" looks like a flag, not a value: flags go before the key, as in `backup set <id> --json <key> <value>`"
            )));
        }
        let mut edit = Self::default();
        match key {
            "name" => edit.name = Some(single(key, values)?),
            "dest" => edit.dest = Some(single(key, values)?),
            "paths" => edit.paths = Some(values.to_vec()),
            "every" => edit.options.every = Some(single(key, values)?),
            "presets" => edit.options.presets = Some(values.to_vec()),
            "filters" => edit.options.filters = Some(values.to_vec()),
            "limit_upload" => edit.options.limit_upload = Some(single(key, values)?),
            "limit_download" => edit.options.limit_download = Some(single(key, values)?),
            "one_file_system" => edit.one_file_system = Some(flag(key, &single(key, values)?)?),
            _ => match key.strip_prefix("keep.") {
                Some(period) => edit
                    .options
                    .keep
                    .set(period, number(key, &single(key, values)?)?)?,
                None => {
                    return Err(OpError::Invalid(format!(
                        "unknown backup setting \"{key}\": use {KEYS}"
                    )));
                }
            },
        }
        Ok(edit)
    }

    /// `before` with this change made. Folders and a local destination
    /// become absolute against `cwd`; `~` is `home`.
    #[must_use]
    pub fn apply(&self, before: &BackupConfig, home: &Path, cwd: &Path) -> BackupConfig {
        let mut after = before.clone();
        if let Some(name) = &self.name {
            after.name.clone_from(name);
        }
        if let Some(paths) = &self.paths {
            after.paths = paths.iter().map(|path| place(path, home, cwd)).collect();
        }
        if let Some(dest) = &self.dest {
            after.dest = place_dest(dest, home, cwd);
        }
        if let Some(flag) = self.one_file_system {
            after.one_file_system = flag;
        }
        self.options.apply_to(&mut after);
        after
    }
}

fn single(key: &str, values: &[String]) -> Result<String, OpError> {
    match values {
        [value] => Ok(value.clone()),
        _ => Err(OpError::Invalid(format!("{key} takes exactly one value"))),
    }
}

fn number(key: &str, text: &str) -> Result<i64, OpError> {
    text.trim().parse().map_err(|_| {
        OpError::Invalid(format!(
            "{key} must be a whole number; got \"{}\"",
            text.trim()
        ))
    })
}

fn flag(key: &str, text: &str) -> Result<bool, OpError> {
    match text {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(OpError::Invalid(format!(
            "{key} must be true or false; got \"{other}\""
        ))),
    }
}

/// `text` as an absolute, normalized path.
pub(super) fn place(text: &str, home: &Path, cwd: &Path) -> PathBuf {
    let expanded = expand_tilde(Path::new(text), home);
    lexical_clean(&cwd.join(expanded))
}

/// A destination written as a path (starting with `~` or `.`) is made
/// absolute; anything else (`/abs`, `account:folder`) stays as written.
fn place_dest(text: &str, home: &Path, cwd: &Path) -> String {
    if text.starts_with(['~', '.']) {
        place(text, home, cwd).to_string_lossy().into_owned()
    } else {
        text.to_owned()
    }
}

/// Refuses the change when it lowers `keys` of safety.
///
/// # Errors
/// [`OpError::UserOnly`] for a new destination alone, [`OpError::NeedsUser`]
/// for anything else.
fn refuse_lowering(keys: &[&str]) -> Result<(), OpError> {
    match keys {
        [] => Ok(()),
        ["dest"] => Err(OpError::UserOnly(
            "A backup destination is fixed. Ask the user to create a new backup.".to_owned(),
        )),
        _ => Err(OpError::NeedsUser(keys.join(", "))),
    }
}

/// What `backup add` and `create_backup` take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewBackupRequest {
    pub name: String,
    pub paths: Vec<String>,
    pub dest: String,
    pub options: BackupOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupCreated {
    pub id: String,
    /// The key is in the key storage; it is never shown here.
    pub key: &'static str,
    /// The person has not yet confirmed saving the key in syncfast.
    pub key_unsaved: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackupSaved {
    pub backup: BackupView,
    pub warnings: Vec<String>,
}

impl Ops {
    /// Creates a backup, with a new key kept in the key storage, and starts
    /// its first run. The backup stays `key_unsaved` until the person saves
    /// the key in syncfast.
    ///
    /// # Errors
    /// The request is not valid or the destination is not empty
    /// ([`OpError::Invalid`]); the key cannot be kept, restic is missing or
    /// the repository cannot be created ([`OpError::Failed`]).
    pub async fn create_backup(&self, request: NewBackupRequest) -> Result<BackupCreated, OpError> {
        let cwd = current_folder()?;
        let restic = self.restic().await?;
        let id = new_backup_id(&self.paths, &request.name).await;
        let blank = NewBackup::with_defaults(String::new(), id, Vec::new(), String::new());
        let edit = BackupEdit {
            name: Some(request.name),
            paths: Some(request.paths),
            dest: Some(request.dest),
            one_file_system: None,
            options: request.options,
        };
        let config = edit.apply(&blank.to_config(), &self.paths.home, &cwd);
        let draft = draft_of(&config).map_err(OpError::Invalid)?;
        let key = Key::generate().map_err(|err| OpError::Failed(err.to_string()))?;
        let env = self.lifecycle_env(restic);
        let created = create_backup(&env, draft, key, true)
            .await
            .map_err(create_error)?;
        let policy = self.load_config().await?.defaults().notifications;
        notify_backup(&*self.notifier, &policy, &config, &created.events);
        Ok(BackupCreated {
            id: created.id,
            key: "stored",
            key_unsaved: true,
            warnings: created.warnings,
        })
    }

    /// Changes a backup, unless the change lowers safety.
    ///
    /// # Errors
    /// The backup does not exist ([`OpError::NoBackup`]); the change lowers
    /// safety ([`OpError::NeedsUser`], [`OpError::UserOnly`]); the values
    /// are not valid ([`OpError::Invalid`]).
    pub async fn edit_backup(&self, id: &str, edit: &BackupEdit) -> Result<BackupSaved, OpError> {
        let cwd = current_folder()?;
        let config = self.load_config().await?;
        let before = config
            .backups()
            .iter()
            .find(|backup| backup.id == id)
            .ok_or_else(|| OpError::NoBackup(id.to_owned()))?;
        if *edit == BackupEdit::default() {
            return Err(OpError::Invalid(format!(
                "nothing to change: give one of {KEYS}"
            )));
        }
        let after = edit.apply(before, &self.paths.home, &cwd);
        refuse_lowering(&backup_lowers_safety(before, &after, &self.paths.home))?;
        let restic = self.restic().await?;
        let applied = save_backup_settings(&self.lifecycle_env(restic), &after)
            .await
            .map_err(|err| match err {
                SaveSettingsError::Invalid(message) => OpError::Invalid(message),
                SaveSettingsError::Unknown(id) => OpError::NoBackup(id),
                SaveSettingsError::Config(message) => OpError::Failed(message),
            })?;
        Ok(BackupSaved {
            backup: self.backup_view(id).await?,
            warnings: applied.warnings,
        })
    }

    /// Removing a backup is for the user, whatever `id` is.
    ///
    /// # Errors
    /// Always [`OpError::UserOnly`].
    pub fn remove_backup(_id: &str) -> Result<(), OpError> {
        Err(OpError::UserOnly(
            "Removing a backup can only be done by the user. Do it in syncfast.".to_owned(),
        ))
    }
}

pub(super) fn current_folder() -> Result<PathBuf, OpError> {
    std::env::current_dir()
        .map_err(|err| OpError::Failed(format!("cannot read the current folder: {err}")))
}

/// The typed draft of a backup config.
fn draft_of(config: &BackupConfig) -> Result<NewBackup, String> {
    let behavior = config.behavior()?;
    let mut draft = NewBackup::with_defaults(
        config.name.clone(),
        config.id.clone(),
        config.paths.clone(),
        config.dest.clone(),
    );
    draft.every = behavior.every;
    draft.keep = behavior.keep;
    draft.presets = behavior.presets;
    draft.filters.clone_from(&config.filters);
    draft.one_file_system = behavior.one_file_system;
    draft.limit_upload = behavior.limit_upload;
    draft.limit_download = behavior.limit_download;
    Ok(draft)
}

fn create_error(err: CreateError) -> OpError {
    match err {
        CreateError::Invalid(message) => OpError::Invalid(message),
        CreateError::Repository(err @ RepositoryError::NotEmpty) => {
            OpError::Invalid(err.to_string())
        }
        other => OpError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::config::KeepSetting;

    const HOME: &str = "/home/user";
    const CWD: &str = "/work";

    fn backup() -> BackupConfig {
        BackupConfig {
            id: "docs".to_owned(),
            name: "Docs".to_owned(),
            paths: vec![PathBuf::from("/home/user/docs")],
            dest: "drive:backups/docs".to_owned(),
            every: "daily".to_owned(),
            keep: KeepSetting::new(7, 4, 6, 0),
            presets: vec!["build_artifacts".to_owned()],
            filters: vec!["- *.tmp".to_owned()],
            one_file_system: true,
            limit_upload: "off".to_owned(),
            limit_download: "off".to_owned(),
            enabled: true,
        }
    }

    fn set(key: &str, values: &[&str]) -> BackupEdit {
        let values: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        BackupEdit::setting(key, &values).unwrap()
    }

    fn after(key: &str, values: &[&str]) -> BackupConfig {
        set(key, values).apply(&backup(), Path::new(HOME), Path::new(CWD))
    }

    fn lowered(key: &str, values: &[&str]) -> Vec<&'static str> {
        backup_lowers_safety(&backup(), &after(key, values), Path::new(HOME))
    }

    #[test]
    fn every_setting_lands_on_its_own_field() {
        assert_eq!(after("name", &["New"]).name, "New");
        assert_eq!(after("every", &["weekly"]).every, "weekly");
        assert_eq!(after("limit_upload", &["1M"]).limit_upload, "1M");
        assert_eq!(after("limit_download", &["2M"]).limit_download, "2M");
        assert_eq!(after("presets", &[]).presets, Vec::<String>::new());
        assert_eq!(after("filters", &["- a", "- b"]).filters, ["- a", "- b"]);
        assert!(!after("one_file_system", &["false"]).one_file_system);
        assert_eq!(after("dest", &["/mnt/x"]).dest, "/mnt/x");
    }

    #[test]
    fn removing_a_backup_is_refused_for_the_user_whatever_the_id() {
        for id in ["docs", "", "nope"] {
            let err = Ops::remove_backup(id).unwrap_err();

            assert!(matches!(err, OpError::UserOnly(_)), "{id}: {err:?}");
            assert_eq!(err.exit_code(), 4);
        }
    }

    #[test]
    fn the_current_folder_is_an_absolute_folder_that_exists() {
        let folder = current_folder().unwrap();

        assert!(folder.is_absolute(), "{}", folder.display());
        assert!(folder.is_dir(), "{}", folder.display());
    }

    #[test]
    fn a_value_that_starts_with_two_dashes_is_a_flag_in_the_wrong_place() {
        for key in ["paths", "filters", "name", "keep.daily"] {
            let err =
                BackupEdit::setting(key, &["/a".to_owned(), "--json".to_owned()]).unwrap_err();

            assert!(matches!(err, OpError::Invalid(_)), "{key}: {err:?}");
            assert!(err.to_string().contains("\"--json\""), "{err}");
            assert!(err.to_string().contains("before the key"), "{err}");
        }
    }

    #[test]
    fn an_exclude_rule_with_a_dash_is_a_value() {
        let edit =
            BackupEdit::setting("filters", &["- *.bak".to_owned(), "+ -x".to_owned()]).unwrap();

        assert_eq!(
            edit.options.filters,
            Some(vec!["- *.bak".to_owned(), "+ -x".to_owned()])
        );
    }

    #[test]
    fn a_keep_value_changes_alone() {
        let changed = after("keep.weekly", &["9"]);

        assert_eq!(changed.keep, KeepSetting::new(7, 9, 6, 0));
    }

    #[test]
    fn folders_become_absolute_and_clean() {
        let changed = after("paths", &["~/a", "./b/../c", "/abs/d/"]);

        assert_eq!(
            changed.paths,
            [
                PathBuf::from("/home/user/a"),
                PathBuf::from("/work/c"),
                PathBuf::from("/abs/d"),
            ]
        );
    }

    #[test]
    fn a_destination_written_as_a_path_becomes_absolute_and_an_account_stays() {
        assert_eq!(after("dest", &["./repo"]).dest, "/work/repo");
        assert_eq!(after("dest", &["~/repo"]).dest, "/home/user/repo");
        assert_eq!(after("dest", &["drive:x/y"]).dest, "drive:x/y");
    }

    #[test]
    fn what_a_request_leaves_out_stays() {
        let changed = set("name", &["New"]).apply(&backup(), Path::new(HOME), Path::new(CWD));

        assert_eq!(
            changed,
            BackupConfig {
                name: "New".to_owned(),
                ..backup()
            }
        );
    }

    #[test]
    fn settings_that_lower_safety_are_named() {
        assert_eq!(lowered("keep.daily", &["3"]), ["keep.daily"]);
        assert_eq!(lowered("keep.yearly", &["0"]), Vec::<&str>::new());
        assert_eq!(lowered("keep.monthly", &["0"]), ["keep.monthly"]);
        assert_eq!(lowered("paths", &[]), ["paths"]);
        assert_eq!(lowered("paths", &["/elsewhere"]), ["paths"]);
        assert_eq!(lowered("dest", &["drive:other"]), ["dest"]);
        assert_eq!(lowered("one_file_system", &["false"]), ["one_file_system"]);
    }

    #[test]
    fn settings_that_do_not_lower_safety_pass() {
        assert!(lowered("keep.daily", &["30"]).is_empty());
        assert!(lowered("paths", &["~/docs", "/more"]).is_empty());
        assert!(lowered("every", &["hourly"]).is_empty());
        assert!(lowered("name", &["Other"]).is_empty());
        assert!(lowered("presets", &[]).is_empty());
        assert!(lowered("filters", &[]).is_empty());
        assert!(lowered("limit_upload", &["1M"]).is_empty());
        assert!(lowered("dest", &["drive:backups/docs/"]).is_empty());
    }

    #[test]
    fn lowering_gives_the_message_the_user_reads() {
        assert!(refuse_lowering(&[]).is_ok());
        assert_eq!(
            refuse_lowering(&["keep.daily", "paths"])
                .unwrap_err()
                .to_string(),
            "This setting lowers safety (keep.daily, paths). Change it in syncfast."
        );
        let dest = refuse_lowering(&["dest"]).unwrap_err();
        assert_eq!(
            dest.to_string(),
            "A backup destination is fixed. Ask the user to create a new backup."
        );
        assert_eq!(dest.exit_code(), 4);
        let both = refuse_lowering(&["paths", "dest"]).unwrap_err();
        assert!(matches!(both, OpError::NeedsUser(_)));
    }

    #[test]
    fn a_setting_with_the_wrong_values_is_invalid() {
        let none: [String; 0] = [];
        let two = ["a".to_owned(), "b".to_owned()];
        let one = ["1".to_owned()];

        for (key, values) in [
            ("name", &none[..]),
            ("every", &two[..]),
            ("one_file_system", &two[..1]),
            ("keep.daily", &two[..1]),
            ("keep.hourly", &one[..]),
            ("colour", &one[..]),
        ] {
            let err = BackupEdit::setting(key, values).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{key}: {err}");
        }
    }

    #[test]
    fn keep_is_parsed_from_pairs() {
        let keep = KeepPartial::parse("daily=7, weekly=4,monthly=6,yearly=1").unwrap();

        assert_eq!(
            keep,
            KeepPartial {
                daily: Some(7),
                weekly: Some(4),
                monthly: Some(6),
                yearly: Some(1),
            }
        );
        assert_eq!(KeepPartial::parse("").unwrap(), KeepPartial::default());
        for bad in ["daily", "daily=x", "hourly=2", "daily=7;weekly=4"] {
            assert!(KeepPartial::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_empty_edit_is_the_default_edit() {
        assert_eq!(
            BackupEdit::default().apply(&backup(), Path::new(HOME), Path::new(CWD)),
            backup()
        );
        assert_ne!(set("name", &["x"]), BackupEdit::default());
    }

    #[test]
    fn removing_a_backup_is_the_users() {
        let err = Ops::remove_backup("docs").unwrap_err();

        assert_eq!(
            err.to_string(),
            "Removing a backup can only be done by the user. Do it in syncfast."
        );
        assert_eq!(err.exit_code(), 4);
        assert_eq!(err.code(), "needs_user");
    }

    #[test]
    fn a_draft_keeps_every_field_of_the_config() {
        let mut config = backup();
        config.limit_upload = "1M".to_owned();
        config.one_file_system = false;

        let draft = draft_of(&config).unwrap();

        assert_eq!(
            draft.to_config(),
            BackupConfig {
                enabled: true,
                ..config
            }
        );
    }

    #[test]
    fn a_draft_of_a_config_with_a_bad_schedule_says_why() {
        let mut config = backup();
        config.every = "yearly".to_owned();

        assert!(draft_of(&config).unwrap_err().contains("unknown schedule"));
    }
}
