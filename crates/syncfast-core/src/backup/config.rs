//! The `[[backup]]` tables of `config.toml`: their types, defaults and
//! checks (spec phase 3, step 2).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

use super::destination::{Destination, DestinationError};
use crate::behavior::{Bandwidth, Behavior};
use crate::config::{
    Pair, expand_tilde, lexical_clean, occupied_remotes, remotes_overlap, resolve, valid_id,
};
use crate::filters;
use crate::paths::Paths;

const KEEP_MAX: i64 = 999;
const SANDBOX_ROOTS: [&str; 2] = ["/run/media", "/mnt"];
pub(crate) const BACKUP_KEYS: [&str; 12] = [
    "id",
    "name",
    "paths",
    "dest",
    "every",
    "keep",
    "presets",
    "filters",
    "one_file_system",
    "limit_upload",
    "limit_download",
    "enabled",
];

/// Why one backup cannot run; the other backups are unaffected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BackupError {
    pub backup_id: String,
    pub message: String,
}

/// Result of [`crate::config::Config::validate_backups`].
#[derive(Debug, Clone, PartialEq)]
pub struct BackupValidation {
    pub backups: Vec<BackupConfig>,
    pub errors: Vec<BackupError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Every {
    Hourly,
    Daily,
    Weekly,
}

impl Every {
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "hourly" => Some(Self::Hourly),
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
        }
    }
}

/// How many snapshots of each period `forget` keeps; 0 turns a period off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep {
    pub daily: u16,
    pub weekly: u16,
    pub monthly: u16,
    pub yearly: u16,
}

impl Default for Keep {
    fn default() -> Self {
        Self {
            daily: 7,
            weekly: 4,
            monthly: 6,
            yearly: 0,
        }
    }
}

/// What a backup does, with the config's text turned into types. The
/// defaults are constants: they do not come from `[defaults]`, which
/// describes syncs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupBehavior {
    pub every: Every,
    pub keep: Keep,
    pub presets: Vec<String>,
    pub one_file_system: bool,
    pub limit_upload: Bandwidth,
    pub limit_download: Bandwidth,
}

impl Default for BackupBehavior {
    fn default() -> Self {
        Self {
            every: Every::Daily,
            keep: Keep::default(),
            presets: default_presets(),
            one_file_system: true,
            limit_upload: Bandwidth::Off,
            limit_download: Bandwidth::Off,
        }
    }
}

/// The `keep` table as written: numbers are checked later, so a value out
/// of range or a key that does not exist is an error of this backup alone.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct KeepSetting {
    #[serde(default)]
    pub daily: i64,
    #[serde(default)]
    pub weekly: i64,
    #[serde(default)]
    pub monthly: i64,
    #[serde(default)]
    pub yearly: i64,
    #[serde(flatten)]
    extra: BTreeMap<String, toml::Value>,
}

impl Default for KeepSetting {
    fn default() -> Self {
        let keep = Keep::default();
        Self {
            daily: keep.daily.into(),
            weekly: keep.weekly.into(),
            monthly: keep.monthly.into(),
            yearly: keep.yearly.into(),
            extra: BTreeMap::new(),
        }
    }
}

impl KeepSetting {
    #[must_use]
    pub fn new(daily: i64, weekly: i64, monthly: i64, yearly: i64) -> Self {
        Self {
            daily,
            weekly,
            monthly,
            yearly,
            extra: BTreeMap::new(),
        }
    }

    fn typed(&self) -> Result<Keep, String> {
        let mut values = [0_u16; 4];
        let named = [
            ("daily", self.daily),
            ("weekly", self.weekly),
            ("monthly", self.monthly),
            ("yearly", self.yearly),
        ];
        for (slot, (name, value)) in values.iter_mut().zip(named) {
            *slot = u16::try_from(value)
                .ok()
                .filter(|value| i64::from(*value) <= KEEP_MAX)
                .ok_or_else(|| format!("keep.{name} must be between 0 and {KEEP_MAX}"))?;
        }
        if let Some(key) = self.extra.keys().next() {
            return Err(format!(
                "unknown keep key \"{key}\": use daily, weekly, monthly or yearly"
            ));
        }
        let [daily, weekly, monthly, yearly] = values;
        if values.iter().all(|value| *value == 0) {
            return Err("keep must retain at least one snapshot".to_owned());
        }
        Ok(Keep {
            daily,
            weekly,
            monthly,
            yearly,
        })
    }
}

/// One `[[backup]]` table.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    pub id: String,
    pub name: String,
    /// `~` already expanded; validation rejects a path that is still
    /// relative.
    pub paths: Vec<PathBuf>,
    /// `<account>:<folder>` or an absolute path (`~` already expanded).
    pub dest: String,
    #[serde(default = "default_every", deserialize_with = "as_text")]
    pub every: String,
    #[serde(default)]
    pub keep: KeepSetting,
    #[serde(default = "default_presets")]
    pub presets: Vec<String>,
    #[serde(default)]
    pub filters: Vec<String>,
    #[serde(default = "default_true")]
    pub one_file_system: bool,
    #[serde(default = "default_limit", deserialize_with = "as_text")]
    pub limit_upload: String,
    #[serde(default = "default_limit", deserialize_with = "as_text")]
    pub limit_download: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

fn default_every() -> String {
    Every::Daily.as_str().to_owned()
}

fn default_limit() -> String {
    Bandwidth::Off.format()
}

fn default_presets() -> Vec<String> {
    filters::preset_names(&filters::DEFAULT_BACKUP_PRESETS)
}

/// A value that is not text becomes its TOML spelling, so the check that
/// follows rejects it with the message of its key instead of a parse error
/// of the whole file.
fn as_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let value = toml::Value::deserialize(deserializer)?;
    Ok(match value {
        toml::Value::String(text) => text,
        other => other.to_string(),
    })
}

fn limit(key: &str, text: &str) -> Result<Bandwidth, String> {
    Bandwidth::parse(text).ok_or_else(|| {
        format!(
            "{key} must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M"
        )
    })
}

/// What is wrong with line `number` (1-based) of a backup's filters.
#[must_use]
pub fn filter_line_problem(number: usize, line: &str) -> Option<String> {
    if line.starts_with("+ ") {
        return Some(format!(
            "backup filters only exclude: remove line {number} (\"+ \")"
        ));
    }
    if !line.starts_with("- ") {
        return Some(format!("filter line {number} must start with \"- \""));
    }
    if line.contains(['\n', '\r']) {
        return Some(format!("filter line {number} must be a single line"));
    }
    if line.contains(['{', '}']) || line.contains("[!") {
        return Some(format!(
            "backup filter line {number} uses \"{{...}}\" or \"[!...]\", which restic does not support"
        ));
    }
    None
}

impl BackupConfig {
    /// The schedule, retention and limits as types.
    ///
    /// # Errors
    /// The first of them that is not valid, as the message the screens and
    /// `syncfast validate` show.
    pub fn behavior(&self) -> Result<BackupBehavior, String> {
        let every = Every::parse(&self.every).ok_or_else(|| {
            format!(
                "unknown schedule \"{}\": use hourly, daily or weekly",
                self.every
            )
        })?;
        let keep = self.keep.typed()?;
        let limit_upload = limit("limit_upload", &self.limit_upload)?;
        let limit_download = limit("limit_download", &self.limit_download)?;
        Ok(BackupBehavior {
            every,
            keep,
            presets: self.presets.clone(),
            one_file_system: self.one_file_system,
            limit_upload,
            limit_download,
        })
    }

    /// What is wrong with the schedule, retention, limits, presets or
    /// filters, in the order of the spec's table.
    fn behavior_problem(&self) -> Option<String> {
        if let Err(message) = self.behavior() {
            return Some(message);
        }
        self.filters_problem()
    }

    /// What is wrong with the presets or the filter lines.
    pub(crate) fn filters_problem(&self) -> Option<String> {
        if let Some(name) = self
            .presets
            .iter()
            .find(|name| filters::preset(name).is_none())
        {
            return Some(format!("unknown preset \"{name}\""));
        }
        self.filters
            .iter()
            .enumerate()
            .find_map(|(index, line)| filter_line_problem(index + 1, line))
    }

    pub(crate) fn name_problem(&self) -> Option<String> {
        (self.name.trim().is_empty() || self.name.chars().any(char::is_control))
            .then(|| "backup name must be one line of text".to_owned())
    }

    fn destination(&self) -> Result<Destination, DestinationError> {
        Destination::parse(&self.dest)
    }
}

/// The `keep.<k>` keys whose value went down, `yearly` to 0 included.
#[must_use]
pub fn keep_reduced(before: &BackupConfig, after: &BackupConfig) -> Vec<&'static str> {
    [
        ("keep.daily", before.keep.daily, after.keep.daily),
        ("keep.weekly", before.keep.weekly, after.keep.weekly),
        ("keep.monthly", before.keep.monthly, after.keep.monthly),
        ("keep.yearly", before.keep.yearly, after.keep.yearly),
    ]
    .into_iter()
    .filter(|(_, before, after)| after < before)
    .map(|(name, _, _)| name)
    .collect()
}

/// `one_file_system` went from on to off.
#[must_use]
pub fn one_file_system_turned_off(before: &BackupConfig, after: &BackupConfig) -> bool {
    before.one_file_system && !after.one_file_system
}

/// The settings a backup change lowers safety on, which only the user may
/// change (spec phase 3, step 15): each `keep` value that went down, a
/// folder that is no longer backed up, a new destination and
/// `one_file_system` turned off. Empty when the change is safe. Folders and
/// the destination compare after `~` and lexical normalization, never
/// touching the disk.
#[must_use]
pub fn backup_lowers_safety(
    before: &BackupConfig,
    after: &BackupConfig,
    home: &Path,
) -> Vec<&'static str> {
    let mut lowered = keep_reduced(before, after);
    let place = |path: &PathBuf| lexical_clean(&expand_tilde(path, home));
    let after_places: Vec<PathBuf> = after.paths.iter().map(place).collect();
    if before
        .paths
        .iter()
        .any(|path| !after_places.contains(&place(path)))
    {
        lowered.push("paths");
    }
    if comparable_dest(&before.dest, home) != comparable_dest(&after.dest, home) {
        lowered.push("dest");
    }
    if one_file_system_turned_off(before, after) {
        lowered.push("one_file_system");
    }
    lowered
}

fn comparable_dest(dest: &str, home: &Path) -> String {
    if dest.starts_with('/') || dest.starts_with('~') {
        lexical_clean(&expand_tilde(Path::new(dest), home))
            .to_string_lossy()
            .into_owned()
    } else {
        dest.trim_end_matches('/').to_owned()
    }
}

pub(crate) fn folder_problem(backup: &BackupConfig) -> Option<String> {
    if backup.enabled && backup.paths.is_empty() {
        return Some("a backup needs at least one folder".to_owned());
    }
    if backup.paths.iter().any(|path| !path.is_absolute()) {
        return Some("backup folder must be an absolute path".to_owned());
    }
    let resolved: Vec<PathBuf> = backup.paths.iter().map(|path| resolve(path)).collect();
    for (i, path) in backup.paths.iter().enumerate() {
        if resolved[..i].contains(&resolved[i]) {
            return Some(format!(
                "backup folder \"{}\" is listed twice",
                path.display()
            ));
        }
    }
    for (i, a) in resolved.iter().enumerate() {
        if let Some(j) = (0..i).find(|&j| a.starts_with(&resolved[j]) || resolved[j].starts_with(a))
        {
            return Some(format!(
                "backup folders overlap: \"{}\" and \"{}\"",
                backup.paths[j].display(),
                backup.paths[i].display()
            ));
        }
    }
    None
}

pub(crate) fn id_problem(id: &str) -> Option<String> {
    (!valid_id(id))
        .then(|| format!("backup id \"{id}\" must use a-z, 0-9 and \"-\", up to 32 characters"))
}

fn problem_before_overlap(
    backup: &BackupConfig,
    duplicate_id: bool,
    accounts: &HashSet<&str>,
) -> Option<String> {
    if let Some(problem) = id_problem(&backup.id) {
        return Some(problem);
    }
    if duplicate_id {
        return Some(format!("duplicate backup id \"{}\"", backup.id));
    }
    if let Some(problem) = backup.name_problem() {
        return Some(problem);
    }
    if let Some(problem) = folder_problem(backup) {
        return Some(problem);
    }
    match backup.destination() {
        Err(err) => Some(err.to_string()),
        Ok(Destination::Account { name, .. }) if !accounts.contains(name.as_str()) => {
            Some(format!("account \"{name}\" is not set up in rclone"))
        }
        Ok(_) => None,
    }
}

/// The destination as `account:folder` text for [`remotes_overlap`], or the
/// resolved local folder.
enum Place {
    Account(String),
    Local(PathBuf),
}

fn place(backup: &BackupConfig) -> Option<Place> {
    match backup.destination().ok()? {
        Destination::Account { name, path } => Some(Place::Account(format!("{name}:{path}"))),
        Destination::Local(path) => Some(Place::Local(resolve(&path))),
    }
}

fn overlap_with_a_sync(place: &Place, pairs: &[(&Pair, Vec<String>, PathBuf)]) -> Option<String> {
    pairs.iter().find_map(|(pair, occupied, local)| {
        let overlaps = match place {
            Place::Account(remote) => occupied.iter().any(|taken| remotes_overlap(remote, taken)),
            Place::Local(folder) => folder.starts_with(local) || local.starts_with(folder),
        };
        overlaps.then(|| format!("backup destination overlaps \"{}\"", pair.name))
    })
}

fn places_overlap(a: &Place, b: &Place) -> bool {
    match (a, b) {
        (Place::Account(a), Place::Account(b)) => remotes_overlap(a, b),
        (Place::Local(a), Place::Local(b)) => a.starts_with(b) || b.starts_with(a),
        _ => false,
    }
}

fn reachable_from_the_sandbox(dest: &str, home: &Path) -> bool {
    let dest = lexical_clean(Path::new(dest));
    dest.starts_with(home) || SANDBOX_ROOTS.iter().any(|root| dest.starts_with(root))
}

/// Everything [`validate_backups`] needs to know about the rest of the
/// config and the machine.
pub(crate) struct Surroundings<'a> {
    pub paths: &'a Paths,
    /// The names of the rclone accounts.
    pub accounts: &'a HashSet<&'a str>,
    pub pairs: &'a [Pair],
    pub defaults: &'a Behavior,
    /// Running inside the Flatpak sandbox.
    pub sandboxed: bool,
}

/// Checks every backup in the order of the spec's table: the first problem
/// of a backup is its error, and the checks that compare backups with each
/// other only look at backups that passed the ones before.
pub(crate) fn validate_backups(
    backups: &[BackupConfig],
    around: &Surroundings<'_>,
) -> BackupValidation {
    let mut messages: Vec<Option<String>> = backups
        .iter()
        .map(|backup| {
            let duplicate = backups.iter().filter(|other| other.id == backup.id).count() > 1;
            problem_before_overlap(backup, duplicate, around.accounts)
        })
        .collect();
    let candidates: Vec<bool> = messages.iter().map(Option::is_none).collect();
    let places: Vec<Option<Place>> = backups
        .iter()
        .zip(&candidates)
        .map(|(backup, &ok)| if ok { place(backup) } else { None })
        .collect();
    let syncs: Vec<(&Pair, Vec<String>, PathBuf)> = around
        .pairs
        .iter()
        .map(|pair| {
            let behavior = Behavior::for_pair(around.defaults, pair);
            (
                pair,
                occupied_remotes(pair, &behavior),
                resolve(&pair.local),
            )
        })
        .collect();
    for (i, backup) in backups.iter().enumerate() {
        let Some(own) = places[i].as_ref().filter(|_| candidates[i]) else {
            continue;
        };
        messages[i] = problem_after_overlap(backup, own, &syncs)
            .or_else(|| {
                backups
                    .iter()
                    .enumerate()
                    .filter(|&(j, _)| j != i)
                    .find(|&(j, _)| {
                        places[j]
                            .as_ref()
                            .is_some_and(|other| places_overlap(own, other))
                    })
                    .map(|(_, other)| format!("backup destination is used by \"{}\"", other.name))
            })
            .or_else(|| sandbox_problem(backup, around))
            .or_else(|| backup.behavior_problem());
    }
    let mut valid = Vec::new();
    let mut errors = Vec::new();
    for (backup, message) in backups.iter().zip(messages) {
        match message {
            None => valid.push(backup.clone()),
            Some(message) => errors.push(BackupError {
                backup_id: backup.id.clone(),
                message,
            }),
        }
    }
    BackupValidation {
        backups: valid,
        errors,
    }
}

fn problem_after_overlap(
    backup: &BackupConfig,
    own: &Place,
    syncs: &[(&Pair, Vec<String>, PathBuf)],
) -> Option<String> {
    if let Some(message) = overlap_with_a_sync(own, syncs) {
        return Some(message);
    }
    if let Place::Local(dest) = own
        && backup
            .paths
            .iter()
            .any(|path| dest.starts_with(resolve(path)))
    {
        return Some("backup destination is inside a folder being backed up".to_owned());
    }
    None
}

fn sandbox_problem(backup: &BackupConfig, around: &Surroundings<'_>) -> Option<String> {
    (around.sandboxed
        && matches!(backup.destination(), Ok(Destination::Local(_)))
        && !reachable_from_the_sandbox(&backup.dest, &around.paths.home))
    .then(|| {
        "backup destination is not reachable from the sandbox; use a folder under your home folder, /run/media or /mnt"
            .to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    const HOME: &str = "/home/ada";

    fn paths() -> Paths {
        Paths::new(
            PathBuf::from(HOME),
            PathBuf::from("/cfg"),
            PathBuf::from("/state"),
            PathBuf::from("/data"),
            PathBuf::from("/run"),
        )
    }

    fn backup_block(extra: &str) -> String {
        format!(
            "[[backup]]\nid = \"docs\"\nname = \"Documents\"\npaths = [\"/work/docs\"]\ndest = \"gdrive:Backups/docs\"\n{extra}"
        )
    }

    fn parse(blocks: &[String]) -> Config {
        let text = format!("version = 3\n\n{}", blocks.join("\n"));
        Config::parse(&text, Path::new(HOME)).unwrap()
    }

    fn check_in(config: &Config, sandboxed: bool) -> BackupValidation {
        let accounts: HashSet<&str> = ["gdrive", "work"].into_iter().collect();
        validate_backups(
            config.backups(),
            &Surroundings {
                paths: &paths(),
                accounts: &accounts,
                pairs: config.pairs(),
                defaults: &config.defaults(),
                sandboxed,
            },
        )
    }

    fn message_of(blocks: &[String]) -> String {
        let result = check_in(&parse(blocks), false);
        result
            .errors
            .first()
            .map(|err| err.message.clone())
            .unwrap_or_default()
    }

    fn one(extra: &str) -> String {
        message_of(&[backup_block(extra)])
    }

    fn sync_block(local: &str, remote: &str) -> String {
        format!(
            "[[pair]]\nid = \"notes\"\nname = \"Notes\"\nlocal = \"{local}\"\nremote = \"{remote}\"\n"
        )
    }

    fn named(id: &str, name: &str, paths: &str, dest: &str) -> String {
        format!(
            "[[backup]]\nid = \"{id}\"\nname = \"{name}\"\npaths = [{paths}]\ndest = \"{dest}\"\n"
        )
    }

    #[test]
    fn a_backup_with_only_the_required_keys_gets_the_defaults() {
        let config = parse(&[backup_block("")]);
        let result = check_in(&config, false);

        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let backup = &result.backups[0];
        assert_eq!(backup.behavior().unwrap(), BackupBehavior::default());
        assert_eq!(backup.every, "daily");
        assert_eq!(backup.keep, KeepSetting::new(7, 4, 6, 0));
        assert_eq!(backup.presets, ["system_junk", "build_artifacts"]);
        assert!(backup.filters.is_empty());
        assert!(backup.one_file_system);
        assert_eq!(backup.limit_upload, "off");
        assert_eq!(backup.limit_download, "off");
        assert!(backup.enabled);
    }

    #[test]
    fn the_defaults_are_the_documented_constants() {
        let behavior = BackupBehavior::default();

        assert_eq!(behavior.every, Every::Daily);
        assert_eq!(
            behavior.keep,
            Keep {
                daily: 7,
                weekly: 4,
                monthly: 6,
                yearly: 0
            }
        );
        assert_eq!(behavior.presets, ["system_junk", "build_artifacts"]);
        assert!(behavior.one_file_system);
        assert_eq!(behavior.limit_upload, Bandwidth::Off);
        assert_eq!(behavior.limit_download, Bandwidth::Off);
    }

    #[test]
    fn every_key_is_read_as_written() {
        let config = parse(&[backup_block(
            "every = \"weekly\"\nkeep = { daily = 1, weekly = 2, monthly = 3, yearly = 4 }\npresets = [\"dev_dirs\"]\nfilters = [\"- *.iso\"]\none_file_system = false\nlimit_upload = \"500K\"\nlimit_download = \"2M\"\nenabled = false\n",
        )]);
        let backup = &config.backups()[0];
        let behavior = backup.behavior().unwrap();

        assert_eq!(behavior.every, Every::Weekly);
        assert_eq!(
            behavior.keep,
            Keep {
                daily: 1,
                weekly: 2,
                monthly: 3,
                yearly: 4
            }
        );
        assert_eq!(behavior.presets, ["dev_dirs"]);
        assert!(!behavior.one_file_system);
        assert_eq!(behavior.limit_upload, Bandwidth::KiBPerSec(500));
        assert_eq!(behavior.limit_download, Bandwidth::KiBPerSec(2048));
        assert_eq!(backup.filters, ["- *.iso"]);
        assert!(!backup.enabled);
    }

    #[test]
    fn a_keep_table_without_a_key_turns_that_period_off() {
        let config = parse(&[backup_block("keep = { weekly = 3 }\n")]);

        assert_eq!(config.backups()[0].keep, KeepSetting::new(0, 3, 0, 0));
        assert!(check_in(&config, false).errors.is_empty());
    }

    #[test]
    fn tilde_in_the_folders_and_a_local_destination_is_expanded() {
        let config = parse(&[
            "[[backup]]\nid = \"docs\"\nname = \"Documents\"\npaths = [\"~/Documents\", \"/etc\"]\ndest = \"~/Backups/docs\"\n"
                .to_owned(),
        ]);
        let backup = &config.backups()[0];

        assert_eq!(
            backup.paths,
            [PathBuf::from("/home/ada/Documents"), PathBuf::from("/etc")]
        );
        assert_eq!(backup.dest, "/home/ada/Backups/docs");
    }

    #[test]
    fn an_account_destination_keeps_its_text() {
        let config = parse(&[backup_block("")]);

        assert_eq!(config.backups()[0].dest, "gdrive:Backups/docs");
    }

    #[test]
    fn an_unknown_key_in_a_backup_is_a_global_error() {
        let text = format!("version = 3\n\n{}", backup_block("hourly = true\n"));

        let err = Config::parse(&text, Path::new(HOME)).unwrap_err();

        assert_eq!(
            err.to_string(),
            "unknown key \"hourly\": not supported by this syncfast version"
        );
    }

    #[test]
    fn a_backup_in_a_version_2_file_is_a_global_error() {
        let text = format!("version = 2\n\n{}", backup_block(""));

        let err = Config::parse(&text, Path::new(HOME)).unwrap_err();

        assert_eq!(err.to_string(), "backups need config version 3");
    }

    #[test]
    fn a_backup_in_a_version_1_file_is_a_global_error() {
        let text = format!("version = 1\n\n{}", backup_block(""));

        let err = Config::parse(&text, Path::new(HOME)).unwrap_err();

        assert_eq!(err.to_string(), "backups need config version 3");
    }

    #[test]
    fn the_id_is_checked() {
        let bad = backup_block("").replace("id = \"docs\"", "id = \"Docs\"");

        assert_eq!(
            message_of(&[bad]),
            "backup id \"Docs\" must use a-z, 0-9 and \"-\", up to 32 characters"
        );
    }

    #[test]
    fn a_repeated_id_fails_every_backup_that_uses_it() {
        let config = parse(&[
            named("docs", "One", "\"/a\"", "gdrive:a"),
            named("docs", "Two", "\"/b\"", "gdrive:b"),
        ]);
        let result = check_in(&config, false);

        assert!(result.backups.is_empty());
        assert_eq!(result.errors.len(), 2);
        assert!(
            result
                .errors
                .iter()
                .all(|err| err.message == "duplicate backup id \"docs\"")
        );
    }

    #[test]
    fn the_name_must_be_one_line() {
        let blank = backup_block("").replace("Documents", "  ");
        let multi = backup_block("").replace("Documents", "a\\nb");

        assert_eq!(message_of(&[blank]), "backup name must be one line of text");
        assert_eq!(message_of(&[multi]), "backup name must be one line of text");
    }

    #[test]
    fn an_enabled_backup_needs_a_folder_and_a_disabled_one_does_not() {
        let none = backup_block("").replace("paths = [\"/work/docs\"]", "paths = []");
        let off = format!("{none}enabled = false\n");

        assert_eq!(message_of(&[none]), "a backup needs at least one folder");
        assert_eq!(message_of(&[off]), "");
    }

    #[test]
    fn a_relative_folder_is_refused() {
        let relative = backup_block("").replace("/work/docs", "docs");

        assert_eq!(
            message_of(&[relative]),
            "backup folder must be an absolute path"
        );
    }

    #[test]
    fn a_folder_listed_twice_is_named() {
        let twice = backup_block("").replace(
            "[\"/work/docs\"]",
            "[\"/work/docs\", \"/work/other\", \"/work/docs/\"]",
        );

        assert_eq!(
            message_of(&[twice]),
            "backup folder \"/work/docs/\" is listed twice"
        );
    }

    #[test]
    fn nested_folders_overlap_in_either_order() {
        let inside = backup_block("").replace("[\"/work/docs\"]", "[\"/work\", \"/work/docs\"]");
        let outside = backup_block("").replace("[\"/work/docs\"]", "[\"/work/docs\", \"/work\"]");

        assert_eq!(
            message_of(&[inside]),
            "backup folders overlap: \"/work\" and \"/work/docs\""
        );
        assert_eq!(
            message_of(&[outside]),
            "backup folders overlap: \"/work/docs\" and \"/work\""
        );
    }

    #[test]
    fn folders_with_a_shared_name_prefix_do_not_overlap() {
        let apart =
            backup_block("").replace("[\"/work/docs\"]", "[\"/work/docs\", \"/work/docs2\"]");

        assert_eq!(message_of(&[apart]), "");
    }

    #[test]
    fn the_destination_form_is_checked() {
        let relative = backup_block("").replace("gdrive:Backups/docs", "backups/docs");
        let root = backup_block("").replace("gdrive:Backups/docs", "gdrive:");

        assert_eq!(
            message_of(&[relative]),
            "backup destination must be an account folder (account:path) or an absolute path"
        );
        assert_eq!(
            message_of(&[root]),
            "backup destination needs a folder inside the account"
        );
    }

    #[test]
    fn an_account_that_rclone_does_not_have_is_named() {
        let unknown = backup_block("").replace("gdrive:", "nowhere:");

        assert_eq!(
            message_of(&[unknown]),
            "account \"nowhere\" is not set up in rclone"
        );
    }

    #[test]
    fn a_destination_on_a_sync_folder_overlaps_it() {
        let inside = message_of(&[sync_block("/n", "gdrive:Backups"), backup_block("")]);
        let around = message_of(&[
            sync_block("/n", "gdrive:Backups/docs/deep"),
            backup_block(""),
        ]);
        let other_account = message_of(&[sync_block("/n", "work:Backups"), backup_block("")]);

        assert_eq!(inside, "backup destination overlaps \"Notes\"");
        assert_eq!(around, "backup destination overlaps \"Notes\"");
        assert_eq!(other_account, "");
    }

    #[test]
    fn the_versions_folder_of_a_mirror_up_sync_counts_as_the_sync() {
        let sync = "[[pair]]\nid = \"notes\"\nname = \"Notes\"\nlocal = \"/n\"\nremote = \"gdrive:Notes\"\nmode = \"mirror_up\"\nversions = true\n";
        let backup = backup_block("").replace("gdrive:Backups/docs", "gdrive:.syncfast-versions");

        let message = message_of(&[sync.to_owned(), backup]);

        assert_eq!(message, "backup destination overlaps \"Notes\"");
    }

    #[test]
    fn a_local_destination_on_a_sync_folder_overlaps_it() {
        let inside = backup_block("").replace("gdrive:Backups/docs", "/n/backups");
        let around = backup_block("").replace("gdrive:Backups/docs", "/");

        assert_eq!(
            message_of(&[sync_block("/n", "work:Notes"), inside]),
            "backup destination overlaps \"Notes\""
        );
        assert_eq!(
            message_of(&[sync_block("/n", "work:Notes"), around]),
            "backup destination overlaps \"Notes\""
        );
    }

    #[test]
    fn a_local_destination_inside_a_backed_up_folder_is_refused() {
        let inside = backup_block("").replace("gdrive:Backups/docs", "/work/docs/repo");

        assert_eq!(
            message_of(&[inside]),
            "backup destination is inside a folder being backed up"
        );
    }

    #[test]
    fn a_destination_shared_with_another_backup_names_that_backup() {
        let config = parse(&[
            named("one", "One", "\"/a\"", "gdrive:Backups"),
            named("two", "Two", "\"/b\"", "gdrive:Backups/two"),
            named("three", "Three", "\"/c\"", "/mnt/disk/three"),
            named("four", "Four", "\"/d\"", "/mnt/disk"),
        ]);
        let result = check_in(&config, false);
        let by_id = |id: &str| {
            result
                .errors
                .iter()
                .find(|err| err.backup_id == id)
                .map(|err| err.message.as_str())
        };

        assert_eq!(by_id("one"), Some("backup destination is used by \"Two\""));
        assert_eq!(by_id("two"), Some("backup destination is used by \"One\""));
        assert_eq!(
            by_id("three"),
            Some("backup destination is used by \"Four\"")
        );
        assert_eq!(
            by_id("four"),
            Some("backup destination is used by \"Three\"")
        );
    }

    #[test]
    fn an_invalid_backup_does_not_block_the_one_that_shares_its_destination() {
        let config = parse(&[
            named("one", "One", "\"relative\"", "gdrive:Backups"),
            named("two", "Two", "\"/b\"", "gdrive:Backups"),
        ]);
        let result = check_in(&config, false);

        assert_eq!(result.backups.len(), 1);
        assert_eq!(result.backups[0].id, "two");
        assert_eq!(
            result.errors[0].message,
            "backup folder must be an absolute path"
        );
    }

    #[test]
    fn inside_the_sandbox_a_local_destination_must_be_where_the_sandbox_can_reach() {
        let reachable = |dest: &str| {
            let block = backup_block("").replace("gdrive:Backups/docs", dest);
            check_in(&parse(&[block]), true).errors
        };

        assert!(reachable("/home/ada/Backups").is_empty());
        assert!(reachable("/run/media/ada/disk/b").is_empty());
        assert!(reachable("/mnt/disk/b").is_empty());
        assert_eq!(
            reachable("/srv/backups")[0].message,
            "backup destination is not reachable from the sandbox; use a folder under your home folder, /run/media or /mnt"
        );
        assert_eq!(
            reachable("/home/ada/../bob/b")[0].message,
            "backup destination is not reachable from the sandbox; use a folder under your home folder, /run/media or /mnt"
        );
        assert!(
            reachable("/home/adam").len() == 1,
            "a name prefix is not home"
        );
    }

    #[test]
    fn outside_the_sandbox_any_local_destination_is_allowed() {
        let block = backup_block("").replace("gdrive:Backups/docs", "/srv/backups");

        assert!(check_in(&parse(&[block]), false).errors.is_empty());
    }

    #[test]
    fn an_account_destination_is_not_limited_by_the_sandbox() {
        assert!(
            check_in(&parse(&[backup_block("")]), true)
                .errors
                .is_empty()
        );
    }

    #[test]
    fn the_schedule_is_checked() {
        assert_eq!(
            one("every = \"monthly\"\n"),
            "unknown schedule \"monthly\": use hourly, daily or weekly"
        );
        assert_eq!(
            one("every = 3\n"),
            "unknown schedule \"3\": use hourly, daily or weekly"
        );
        assert_eq!(one("every = \"hourly\"\n"), "");
    }

    #[test]
    fn keep_values_are_between_0_and_999() {
        assert_eq!(
            one("keep = { daily = 1000 }\n"),
            "keep.daily must be between 0 and 999"
        );
        assert_eq!(
            one("keep = { daily = 1, monthly = -1 }\n"),
            "keep.monthly must be between 0 and 999"
        );
        assert_eq!(one("keep = { daily = 999 }\n"), "");
    }

    #[test]
    fn a_keep_key_that_does_not_exist_is_named() {
        assert_eq!(
            one("keep = { daily = 1, last = 3 }\n"),
            "unknown keep key \"last\": use daily, weekly, monthly or yearly"
        );
        assert_eq!(
            one("keep = { hourly = 3 }\n"),
            "unknown keep key \"hourly\": use daily, weekly, monthly or yearly"
        );
    }

    #[test]
    fn keep_must_retain_something() {
        assert_eq!(
            one("keep = { daily = 0, weekly = 0, monthly = 0, yearly = 0 }\n"),
            "keep must retain at least one snapshot"
        );
        assert_eq!(one("keep = {}\n"), "keep must retain at least one snapshot");
        assert_eq!(one("keep = { yearly = 1 }\n"), "");
    }

    #[test]
    fn a_limit_that_is_not_a_speed_names_its_key() {
        assert_eq!(
            one("limit_upload = \"fast\"\n"),
            "limit_upload must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M"
        );
        assert_eq!(
            one("limit_download = 5\n"),
            "limit_download must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M"
        );
        assert!(one("limit_upload = \"0K\"\n").starts_with("limit_upload must be off"));
    }

    #[test]
    fn a_preset_that_does_not_exist_is_named() {
        assert_eq!(
            one("presets = [\"system_junk\", \"nope\"]\n"),
            "unknown preset \"nope\""
        );
        assert_eq!(one("presets = [\"git_repos\", \"dev_dirs\"]\n"), "");
        assert_eq!(one("presets = []\n"), "");
    }

    #[test]
    fn filters_only_exclude_and_only_with_what_restic_understands() {
        assert_eq!(
            one("filters = [\"- *.iso\", \"*.tmp\"]\n"),
            "filter line 2 must start with \"- \""
        );
        assert_eq!(
            one("filters = [\"+ keep.txt\"]\n"),
            "backup filters only exclude: remove line 1 (\"+ \")"
        );
        for line in ["- *.{jpg,png}", "- a}", "- [!a]*"] {
            assert_eq!(
                one(&format!("filters = [\"{line}\"]\n")),
                "backup filter line 1 uses \"{...}\" or \"[!...]\", which restic does not support",
                "{line}"
            );
        }
        assert_eq!(one("filters = [\"- [ab]*\", \"- **/cache\"]\n"), "");
    }

    #[test]
    fn the_first_problem_in_the_table_order_wins() {
        let both = backup_block("every = \"never\"\nkeep = { daily = 0 }\npresets = [\"x\"]\n");

        assert_eq!(
            message_of(&[both]),
            "unknown schedule \"never\": use hourly, daily or weekly"
        );
        let later = backup_block("keep = { daily = 0 }\nlimit_upload = \"x\"\npresets = [\"x\"]\n");
        assert_eq!(
            message_of(&[later]),
            "keep must retain at least one snapshot"
        );
    }

    #[test]
    fn keep_may_be_a_table_of_its_own() {
        let config = parse(&[format!("{}\n[backup.keep]\ndaily = 2\n", backup_block(""))]);

        assert_eq!(config.backups()[0].keep, KeepSetting::new(2, 0, 0, 0));
    }

    fn lowers(before: &str, after: &str) -> Vec<&'static str> {
        let before = parse(&[backup_block(before)]).backups()[0].clone();
        let after = parse(&[backup_block(after)]).backups()[0].clone();
        guard(&before, &after)
    }

    fn guard(before: &BackupConfig, after: &BackupConfig) -> Vec<&'static str> {
        backup_lowers_safety(before, after, Path::new(HOME))
    }

    #[test]
    fn a_lower_keep_lowers_safety_for_each_period() {
        assert_eq!(
            lowers(
                "",
                "keep = { daily = 6, weekly = 3, monthly = 5, yearly = 0 }\n"
            ),
            ["keep.daily", "keep.weekly", "keep.monthly"]
        );
        assert_eq!(
            lowers(
                "keep = { daily = 1, yearly = 2 }\n",
                "keep = { daily = 1, yearly = 0 }\n"
            ),
            ["keep.yearly"]
        );
    }

    #[test]
    fn a_higher_or_equal_keep_is_safe() {
        assert!(
            lowers(
                "",
                "keep = { daily = 7, weekly = 4, monthly = 6, yearly = 1 }\n"
            )
            .is_empty()
        );
        assert!(lowers("", "keep = { daily = 70, weekly = 40, monthly = 60 }\n").is_empty());
        assert!(lowers("", "").is_empty());
    }

    #[test]
    fn a_folder_that_is_no_longer_backed_up_lowers_safety() {
        let before = parse(&[backup_block("")]).backups()[0].clone();
        let mut removed = before.clone();
        removed.paths.clear();
        let mut swapped = before.clone();
        swapped.paths = vec![PathBuf::from("/other")];
        let mut added = before.clone();
        added.paths.push(PathBuf::from("/other"));

        assert_eq!(guard(&before, &removed), ["paths"]);
        assert_eq!(guard(&before, &swapped), ["paths"]);
        assert!(guard(&before, &added).is_empty());
    }

    #[test]
    fn a_new_destination_lowers_safety() {
        let before = parse(&[backup_block("")]).backups()[0].clone();
        let mut after = before.clone();
        after.dest = "gdrive:Elsewhere".to_owned();

        assert_eq!(guard(&before, &after), ["dest"]);
        assert!(guard(&before, &before).is_empty());
    }

    #[test]
    fn the_same_folders_written_another_way_are_not_removed() {
        let mut before = parse(&[backup_block("")]).backups()[0].clone();
        before.paths = vec![PathBuf::from("/home/ada/Documents"), PathBuf::from("/etc")];
        before.dest = "/home/ada/Backups/docs".to_owned();
        let mut after = before.clone();
        after.paths = vec![
            PathBuf::from("~/Documents/"),
            PathBuf::from("/etc/./"),
            PathBuf::from("/home//ada/Documents/../Documents"),
        ];
        after.dest = "~/Backups//docs/".to_owned();

        assert!(guard(&before, &after).is_empty());

        after.paths = vec![PathBuf::from("~/Docs"), PathBuf::from("/etc")];
        assert_eq!(guard(&before, &after), ["paths"]);
    }

    #[test]
    fn an_account_folder_with_a_trailing_slash_is_the_same_destination() {
        let mut before = parse(&[backup_block("")]).backups()[0].clone();
        before.dest = "gdrive:Backups/docs".to_owned();
        let mut after = before.clone();
        after.dest = "gdrive:Backups/docs/".to_owned();

        assert!(guard(&before, &after).is_empty());

        after.dest = "gdrive:Backups/other".to_owned();
        assert_eq!(guard(&before, &after), ["dest"]);
    }

    #[test]
    fn turning_one_file_system_off_lowers_safety_and_on_does_not() {
        assert_eq!(lowers("", "one_file_system = false\n"), ["one_file_system"]);
        assert!(lowers("one_file_system = false\n", "").is_empty());
        assert!(lowers("one_file_system = false\n", "one_file_system = false\n").is_empty());
    }

    #[test]
    fn several_changes_are_all_reported_in_a_fixed_order() {
        let before = parse(&[backup_block("")]).backups()[0].clone();
        let mut after = before.clone();
        after.keep = KeepSetting::new(1, 4, 6, 0);
        after.paths.clear();
        after.dest = "/mnt/x".to_owned();
        after.one_file_system = false;

        assert_eq!(
            guard(&before, &after),
            ["keep.daily", "paths", "dest", "one_file_system"]
        );
    }
}
