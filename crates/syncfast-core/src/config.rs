//! `$XDG_CONFIG_HOME/syncfast/config.toml`: parsing, validation and the
//! comment-preserving edits the GUI performs.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, TableLike, Value};

use crate::behavior::Behavior;
use crate::filters;
use crate::paths::Paths;
use crate::settings::{self, Effective, Origin, Overrides};

const SUPPORTED_VERSION: i64 = 2;
const FIRST_VERSION: i64 = 1;
const V1_TOP_LEVEL_KEYS: [&str; 2] = ["version", "pair"];
const TOP_LEVEL_KEYS: [&str; 5] = ["version", "defaults", "notifications", "app", "pair"];
const APP_TABLES: [&str; 2] = ["notifications", "app"];
const PAIR_KEYS: [&str; 7] = [
    "id", "name", "local", "remote", "enabled", "presets", "filters",
];
const V1_BACKUP: &str = "config.toml.v1";

/// Problems that stop the whole file from loading.
#[derive(Debug, thiserror::Error)]
pub enum GlobalConfigError {
    #[error("config.toml: {0}")]
    Toml(String),
    #[error("config.toml: {0}")]
    Read(std::io::Error),
    #[error("unknown key \"{0}\": not supported by this syncfast version")]
    UnknownKey(String),
    #[error("unsupported config version {0}")]
    UnsupportedVersion(i64),
    #[error("config.toml: [defaults]: {0}")]
    Defaults(String),
    #[error("config.toml: {0}")]
    App(String),
}

/// Why one pair cannot run; the other pairs are unaffected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PairError {
    pub pair_id: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigSaveError {
    #[error("cannot write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot lock {}: {source}", path.display())]
    Lock {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config.toml is a link to a missing file: {}", target.display())]
    DanglingLink { target: PathBuf },
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigUpdateError {
    #[error(transparent)]
    Load(#[from] GlobalConfigError),
    #[error(transparent)]
    Save(#[from] ConfigSaveError),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigEditError {
    #[error("local folder path is not valid UTF-8")]
    NonUtf8Path,
}

/// Where a setting is written: `[defaults]` or one sync's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingScope {
    Defaults,
    Sync(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    #[error("unknown setting \"{0}\"")]
    UnknownKey(String),
    #[error("no sync with id \"{0}\"")]
    UnknownSync(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Pair {
    pub id: String,
    pub name: String,
    /// `~` already expanded; validation rejects it when still relative.
    pub local: PathBuf,
    /// `<account>:<folder>`, for example `gdrive:` or `gdrive:Notes`.
    pub remote: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_presets")]
    pub presets: Vec<String>,
    #[serde(default)]
    pub filters: Vec<String>,
    /// The behavior keys this sync's own table sets.
    #[serde(skip)]
    pub overrides: Overrides,
}

fn default_enabled() -> bool {
    true
}

fn default_presets() -> Vec<String> {
    filters::PRESETS
        .iter()
        .map(|preset| preset.name.to_owned())
        .collect()
}

/// Splits `account:folder` at the first colon. `None` when there is no colon
/// or the account is empty.
#[must_use]
pub fn split_remote(remote: &str) -> Option<(&str, &str)> {
    remote
        .split_once(':')
        .filter(|(account, _)| !account.is_empty())
}

#[derive(Debug, Deserialize)]
struct Raw {
    #[serde(rename = "version")]
    _version: i64,
    #[serde(default)]
    pair: Vec<Pair>,
}

/// The loaded file. Keeps the TOML document so edits and [`Config::save`]
/// preserve comments and ordering.
#[derive(Debug, Clone)]
pub struct Config {
    document: DocumentMut,
    pairs: Vec<Pair>,
    defaults: Overrides,
}

/// Result of [`Config::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    pub pairs: Vec<Pair>,
    pub errors: Vec<PairError>,
}

impl Config {
    #[must_use]
    pub fn empty() -> Self {
        let mut document = DocumentMut::new();
        document["version"] = toml_edit::value(SUPPORTED_VERSION);
        Self {
            document,
            pairs: Vec::new(),
            defaults: Overrides::default(),
        }
    }

    /// Reads `config.toml`; an absent file is an empty config.
    ///
    /// # Errors
    /// The file cannot be read or has a global problem (see [`Config::parse`]).
    pub async fn load(paths: &Paths) -> Result<Self, GlobalConfigError> {
        match tokio::fs::read_to_string(paths.config_file()).await {
            Ok(text) => Self::parse(&text, &paths.home),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(err) => Err(GlobalConfigError::Read(err)),
        }
    }

    /// Parses `text`, expanding a leading `~` in each `local` with `home`.
    ///
    /// # Errors
    /// Invalid TOML or types, an unsupported `version`, or an unknown key.
    pub fn parse(text: &str, home: &Path) -> Result<Self, GlobalConfigError> {
        let mut document: DocumentMut = text
            .parse()
            .map_err(|err: toml_edit::TomlError| GlobalConfigError::Toml(err.to_string()))?;
        let version = document.get("version").and_then(Item::as_integer);
        if let Some(version) = version
            && version != FIRST_VERSION
            && version != SUPPORTED_VERSION
        {
            return Err(GlobalConfigError::UnsupportedVersion(version));
        }
        normalize_pair_array(&mut document);
        reject_unknown_keys(&document, version.unwrap_or(SUPPORTED_VERSION))?;
        let defaults = read_settings(&document)?;
        let mut raw: Raw =
            toml::from_str(text).map_err(|err| GlobalConfigError::Toml(err.to_string()))?;
        for pair in &mut raw.pair {
            pair.local = expand_tilde(&pair.local, home);
        }
        let mut config = Self {
            document,
            pairs: raw.pair,
            defaults,
        };
        config.refresh_overrides();
        Ok(config)
    }

    /// The version the file was written with when it is older than the
    /// current one: it migrates on the next save.
    #[must_use]
    pub fn migrated_from(&self) -> Option<i64> {
        self.document
            .get("version")
            .and_then(Item::as_integer)
            .filter(|&version| version < SUPPORTED_VERSION)
    }

    /// Rewrites an older `config.toml` as the current version, keeping the
    /// original beside it as `config.toml.v1`. Does nothing for a file that
    /// is current, absent or does not load. Returns whether it migrated.
    ///
    /// # Errors
    /// The lock, the copy or the save fails.
    pub async fn migrate(paths: &Paths) -> Result<bool, ConfigUpdateError> {
        let _lock = lock_config(paths).await?;
        let Ok(config) = Self::load(paths).await else {
            return Ok(false);
        };
        if config.migrated_from().is_none() {
            return Ok(false);
        }
        let target = real_config_file(paths).await?;
        let backup = target.with_file_name(V1_BACKUP);
        if !tokio::fs::try_exists(&backup).await.unwrap_or(false) {
            tokio::fs::copy(&target, &backup)
                .await
                .map_err(|source| ConfigSaveError::Write {
                    path: backup,
                    source,
                })?;
        }
        config.save(paths).await?;
        Ok(true)
    }

    /// The built-in values with `[defaults]` on top.
    #[must_use]
    pub fn defaults(&self) -> Behavior {
        let mut behavior = Behavior::default();
        self.defaults.apply_to(&mut behavior);
        behavior
    }

    /// The keys `[defaults]` sets.
    #[must_use]
    pub fn defaults_overrides(&self) -> &Overrides {
        &self.defaults
    }

    /// What a sync runs with and where each value comes from.
    #[must_use]
    pub fn effective(&self, pair: &Pair) -> Effective {
        let origins = settings::KEYS
            .iter()
            .map(|def| {
                let origin = if pair.overrides.contains(def.name) {
                    Origin::Sync
                } else if self.defaults.contains(def.name) {
                    Origin::Defaults
                } else {
                    Origin::Builtin
                };
                (def.name, origin)
            })
            .collect();
        Effective {
            behavior: Behavior::for_pair(&self.defaults(), pair),
            origins,
        }
    }

    #[must_use]
    pub fn pairs(&self) -> &[Pair] {
        &self.pairs
    }

    /// Checks every pair. `accounts` are the rclone remote names (a trailing
    /// `:` is accepted, so the output of `rclone listremotes` can be passed
    /// as is). Touches the filesystem only to canonicalize local folders.
    #[must_use]
    pub fn validate(&self, paths: &Paths, accounts: &[String]) -> Validation {
        let accounts: HashSet<&str> = accounts
            .iter()
            .map(|account| account.trim_end_matches(':'))
            .collect();
        let mut id_counts: HashMap<&str, usize> = HashMap::new();
        for pair in &self.pairs {
            *id_counts.entry(pair.id.as_str()).or_default() += 1;
        }
        let data_dir = resolve(&paths.data_dir());
        let defaults = self.defaults();
        let mut messages: Vec<Option<String>> = self
            .pairs
            .iter()
            .map(|pair| {
                let duplicate = id_counts.get(pair.id.as_str()).copied().unwrap_or(0) > 1;
                problem_before_overlap(pair, duplicate, &data_dir, &defaults)
            })
            .collect();
        let candidates: Vec<bool> = messages.iter().map(Option::is_none).collect();
        let locals: Vec<PathBuf> = self
            .pairs
            .iter()
            .zip(&candidates)
            .map(|(pair, &ok)| {
                if ok {
                    resolve(&pair.local)
                } else {
                    PathBuf::new()
                }
            })
            .collect();
        for (i, pair) in self.pairs.iter().enumerate() {
            if !candidates[i] {
                continue;
            }
            let others = || {
                self.pairs
                    .iter()
                    .enumerate()
                    .filter(|&(j, _)| j != i && candidates[j])
            };
            let local_overlap = others().find(|&(j, _)| {
                locals[i].starts_with(&locals[j]) || locals[j].starts_with(&locals[i])
            });
            messages[i] = if let Some((_, other)) = local_overlap {
                Some(format!("local folder overlaps \"{}\"", other.name))
            } else if let Some((_, other)) =
                others().find(|(_, other)| remotes_overlap(&pair.remote, &other.remote))
            {
                Some(format!("remote folder overlaps \"{}\"", other.name))
            } else {
                problem_after_overlap(pair, &accounts)
            };
        }
        let mut valid = Vec::new();
        let mut errors = Vec::new();
        for (pair, message) in self.pairs.iter().zip(messages) {
            match message {
                None => valid.push(pair.clone()),
                Some(message) => errors.push(PairError {
                    pair_id: pair.id.clone(),
                    message,
                }),
            }
        }
        Validation {
            pairs: valid,
            errors,
        }
    }

    /// # Errors
    /// The local path is not valid UTF-8, so TOML cannot hold it.
    pub fn add_pair(&mut self, pair: Pair) -> Result<(), ConfigEditError> {
        let local = pair.local.to_str().ok_or(ConfigEditError::NonUtf8Path)?;
        let mut table = Table::new();
        table.decor_mut().set_prefix("\n");
        table["id"] = toml_edit::value(pair.id.as_str());
        table["name"] = toml_edit::value(pair.name.as_str());
        table["local"] = toml_edit::value(local);
        table["remote"] = toml_edit::value(pair.remote.as_str());
        table["enabled"] = toml_edit::value(pair.enabled);
        table["presets"] = Item::Value(string_array(&pair.presets));
        table["filters"] = Item::Value(string_array(&pair.filters));
        let item = self.document.entry("pair").or_insert(Item::None);
        if let Item::ArrayOfTables(tables) = item {
            tables.push(table);
        } else {
            let mut tables = ArrayOfTables::new();
            tables.push(table);
            *item = Item::ArrayOfTables(tables);
        }
        self.pairs.push(pair);
        Ok(())
    }

    pub fn remove_pair(&mut self, id: &str) -> bool {
        let before = self.pairs.len();
        self.pairs.retain(|pair| pair.id != id);
        if let Some(tables) = self
            .document
            .get_mut("pair")
            .and_then(Item::as_array_of_tables_mut)
        {
            tables.retain(|table| table_id(table) != Some(id));
            if tables.is_empty() {
                self.document.remove("pair");
            }
        }
        self.pairs.len() != before
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        self.edit_pair(
            id,
            |pair| pair.enabled = enabled,
            |table| {
                replace_value(table, "enabled", Value::from(enabled));
            },
        )
    }

    pub fn set_filters(&mut self, id: &str, presets: &[String], filters: &[String]) -> bool {
        self.edit_pair(
            id,
            |pair| {
                pair.presets = presets.to_vec();
                pair.filters = filters.to_vec();
            },
            |table| {
                replace_value(table, "presets", string_array(presets));
                replace_value(table, "filters", string_array(filters));
            },
        )
    }

    /// Points every sync on the account `from` at `to`, keeping the folder
    /// and the comments. Returns the ids of the syncs it changed, in config
    /// order.
    pub fn rename_account(&mut self, from: &str, to: &str) -> Vec<String> {
        let moved: Vec<(String, String)> = self
            .pairs
            .iter()
            .filter_map(|pair| {
                let (account, folder) = split_remote(&pair.remote)?;
                (account == from).then(|| (pair.id.clone(), format!("{to}:{folder}")))
            })
            .collect();
        for (id, remote) in &moved {
            self.edit_pair(
                id,
                |pair| pair.remote.clone_from(remote),
                |table| replace_value(table, "remote", Value::from(remote.as_str())),
            );
        }
        moved.into_iter().map(|(id, _)| id).collect()
    }

    /// The file contents with comments and ordering intact.
    #[must_use]
    pub fn to_toml(&self) -> String {
        self.document.to_string()
    }

    /// Applies `change` under an exclusive lock and saves, so concurrent edits
    /// never overwrite each other; nothing is saved if it changes nothing.
    ///
    /// # Errors
    /// The lock, the load or the save fails.
    pub async fn edit<T>(
        paths: &Paths,
        change: impl FnOnce(&mut Self) -> T,
    ) -> Result<T, ConfigUpdateError> {
        let _lock = lock_config(paths).await?;
        let mut config = Self::load(paths).await?;
        let before = config.to_toml();
        let outcome = change(&mut config);
        if config.to_toml() != before {
            config.save(paths).await?;
        }
        Ok(outcome)
    }

    /// Writes `config.toml` atomically. A symlinked file (GNU stow) is
    /// followed, so the link survives and its target is updated.
    ///
    /// # Errors
    /// The file cannot be written, or is a link to a file that does not
    /// exist (writing would replace the link).
    pub async fn save(&self, paths: &Paths) -> Result<(), ConfigSaveError> {
        let target = real_config_file(paths).await?;
        let contents = self.saved_text();
        let written = target.clone();
        let outcome =
            tokio::task::spawn_blocking(move || fastframe_file::write(&written, contents))
                .await
                .unwrap_or_else(|join| Err(std::io::Error::other(join)));
        outcome.map_err(|source| ConfigSaveError::Write {
            path: target,
            source,
        })
    }

    /// What `save` writes: the document, with the version line brought up to
    /// date and nothing else touched.
    fn saved_text(&self) -> String {
        if self.migrated_from().is_none() {
            return self.to_toml();
        }
        let mut document = self.document.clone();
        replace_value(
            document.as_table_mut(),
            "version",
            Value::from(SUPPORTED_VERSION),
        );
        document.to_string()
    }

    /// Sets `key` in `scope` to the value `text` spells, after checking it
    /// as a whole: nothing changes when the value or the combination it makes
    /// is not valid.
    ///
    /// # Errors
    /// The key or the sync does not exist, or the value is not valid.
    pub fn set_setting(
        &mut self,
        scope: &SettingScope,
        key: &str,
        text: &str,
    ) -> Result<(), SettingError> {
        let def = scoped_key(scope, key)?;
        if *scope == SettingScope::Defaults && def.per_sync_only {
            return Err(SettingError::Invalid(settings::per_sync_only_message(key)));
        }
        let value = def.parse_text(text).map_err(SettingError::Invalid)?;
        self.change_setting(scope, def, Some(&value))
    }

    /// Removes `key` from `scope`, so the value comes from further up.
    ///
    /// # Errors
    /// The key or the sync does not exist, or the value it falls back to
    /// makes an invalid combination.
    pub fn reset_setting(&mut self, scope: &SettingScope, key: &str) -> Result<(), SettingError> {
        let def = scoped_key(scope, key)?;
        self.change_setting(scope, def, None)
    }

    fn change_setting(
        &mut self,
        scope: &SettingScope,
        def: &settings::KeyDef,
        value: Option<&settings::Value>,
    ) -> Result<(), SettingError> {
        let mut next = self.clone();
        next.write_setting(scope, def, value)?;
        if let Some(problem) = next.new_relation_problem(self) {
            return Err(SettingError::Invalid(problem));
        }
        *self = next;
        Ok(())
    }

    fn write_setting(
        &mut self,
        scope: &SettingScope,
        def: &settings::KeyDef,
        value: Option<&settings::Value>,
    ) -> Result<(), SettingError> {
        match scope {
            SettingScope::Defaults => {
                let section = def
                    .name
                    .split_once('.')
                    .map_or("defaults", |(table, _)| table);
                let table = match value {
                    Some(_) => self.section_table_mut(section),
                    None => self
                        .document
                        .get_mut(section)
                        .and_then(Item::as_table_like_mut),
                };
                if let Some(table) = table {
                    write_or_remove(table, def, value);
                }
                if section != "defaults"
                    && self
                        .document
                        .get(section)
                        .and_then(Item::as_table_like)
                        .is_some_and(TableLike::is_empty)
                {
                    self.document.remove(section);
                }
            }
            SettingScope::Sync(id) => {
                if !self.pairs.iter().any(|pair| pair.id == *id) {
                    return Err(SettingError::UnknownSync(id.clone()));
                }
                if let Some(tables) = self
                    .document
                    .get_mut("pair")
                    .and_then(Item::as_array_of_tables_mut)
                {
                    for table in tables.iter_mut().filter(|t| table_id(t) == Some(id)) {
                        write_or_remove(table, def, value);
                    }
                }
            }
        }
        self.refresh_overrides();
        Ok(())
    }

    /// `[defaults]`, `[notifications]` and `[app]`, in that order, go right
    /// after `version`, ahead of the syncs.
    fn section_table_mut(&mut self, name: &str) -> Option<&mut dyn TableLike> {
        if !self.document.contains_key(name) {
            let mut table = Table::new();
            table.decor_mut().set_prefix("\n");
            self.document.insert(name, Item::Table(table));
            self.order_tables();
        }
        self.document.get_mut(name)?.as_table_like_mut()
    }

    fn order_tables(&mut self) {
        let mut next = 0;
        for name in ["defaults", "notifications", "app"] {
            if let Some(table) = self.document.get_mut(name).and_then(Item::as_table_mut) {
                table.set_position(next);
                next += 1;
            }
        }
        let pairs = self
            .document
            .get_mut("pair")
            .and_then(Item::as_array_of_tables_mut);
        for table in pairs.into_iter().flat_map(ArrayOfTables::iter_mut) {
            table.set_position(next);
            next += 1;
        }
    }

    /// Reads the behavior keys again from the document, the one place they
    /// are kept.
    fn refresh_overrides(&mut self) {
        let defaults = self
            .document
            .get("defaults")
            .and_then(Item::as_table_like)
            .map_or_else(Overrides::default, |table| Overrides::read(table, true));
        self.defaults = Overrides::read_app(self.document.as_table()).merged_over(&defaults);
        if let Some(tables) = self.document.get("pair").and_then(Item::as_array_of_tables) {
            for (pair, table) in self.pairs.iter_mut().zip(tables) {
                pair.overrides = Overrides::read(table, false);
            }
        }
    }

    fn relation_problems(&self) -> Vec<(Option<String>, String)> {
        let defaults = self.defaults();
        let mut found = Vec::new();
        if let Some(message) = settings::relation_problem(&defaults) {
            found.push((None, message));
        }
        for pair in &self.pairs {
            if let Some(message) = settings::relation_problem(&Behavior::for_pair(&defaults, pair))
            {
                found.push((Some(pair.id.clone()), message));
            }
        }
        found
    }

    /// A relation problem this config has and `before` did not.
    fn new_relation_problem(&self, before: &Self) -> Option<String> {
        let old = before.relation_problems();
        self.relation_problems()
            .into_iter()
            .find(|problem| !old.contains(problem))
            .map(|(_, message)| message)
    }

    fn edit_pair(
        &mut self,
        id: &str,
        on_pair: impl Fn(&mut Pair),
        on_table: impl Fn(&mut Table),
    ) -> bool {
        let mut found = false;
        for pair in self.pairs.iter_mut().filter(|pair| pair.id == id) {
            on_pair(pair);
            found = true;
        }
        if let Some(tables) = self
            .document
            .get_mut("pair")
            .and_then(Item::as_array_of_tables_mut)
        {
            for table in tables.iter_mut().filter(|t| table_id(t) == Some(id)) {
                on_table(table);
            }
        }
        found
    }
}

/// Blocks until this process holds `config.lock` exclusively; the lock is
/// released when the returned file is dropped. It lives in the state folder
/// because the config folder may be a link into a dotfiles repo.
async fn lock_config(paths: &Paths) -> Result<std::fs::File, ConfigSaveError> {
    let dir = paths.state_dir();
    let path = dir.join("config.lock");
    let locked = path.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<std::fs::File> {
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&locked)?;
        file.lock()?;
        Ok(file)
    })
    .await
    .unwrap_or_else(|join| Err(std::io::Error::other(join)))
    .map_err(|source| ConfigSaveError::Lock { path, source })
}

fn table_id(table: &Table) -> Option<&str> {
    table.get("id").and_then(Item::as_str)
}

fn string_array(items: &[String]) -> Value {
    Value::Array(items.iter().map(String::as_str).collect::<Array>())
}

/// Swaps a value in place, keeping the key's comments and the value's
/// trailing comment.
fn replace_value(table: &mut dyn TableLike, key: &str, mut new: Value) {
    match table.get_mut(key) {
        Some(item) => {
            if let Some(old) = item.as_value() {
                *new.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(new);
        }
        None => {
            table.insert(key, Item::Value(new));
        }
    }
}

/// Turns an inline `pair = [{ ... }]` into `[[pair]]` tables so edits find it.
fn normalize_pair_array(document: &mut DocumentMut) {
    let Some(item) = document.get_mut("pair") else {
        return;
    };
    if item.is_array() {
        *item = match std::mem::take(item).into_array_of_tables() {
            Ok(tables) => Item::ArrayOfTables(tables),
            Err(original) => original,
        };
    }
}

/// The values `[defaults]`, `[notifications]` and `[app]` set, together.
fn read_settings(document: &DocumentMut) -> Result<Overrides, GlobalConfigError> {
    let defaults = read_defaults(document)?;
    for section in APP_TABLES {
        if document
            .get(section)
            .is_some_and(|item| item.as_table_like().is_none())
        {
            return Err(GlobalConfigError::Toml(format!(
                "[{section}] must be a table"
            )));
        }
    }
    let app = Overrides::read_app(document.as_table());
    match app.problem() {
        Some(message) => Err(GlobalConfigError::App(message.to_owned())),
        None => Ok(app.merged_over(&defaults)),
    }
}

fn scoped_key(scope: &SettingScope, key: &str) -> Result<&'static settings::KeyDef, SettingError> {
    if let Some(def) = settings::key(key) {
        return Ok(def);
    }
    let def = settings::app_key(key).ok_or_else(|| SettingError::UnknownKey(key.to_owned()))?;
    if matches!(scope, SettingScope::Sync(_)) {
        return Err(SettingError::Invalid(format!(
            "{key} applies to the whole app, not to one sync"
        )));
    }
    Ok(def)
}

fn read_defaults(document: &DocumentMut) -> Result<Overrides, GlobalConfigError> {
    let Some(item) = document.get("defaults") else {
        return Ok(Overrides::default());
    };
    let table = item
        .as_table_like()
        .ok_or_else(|| GlobalConfigError::Toml("[defaults] must be a table".to_owned()))?;
    let overrides = Overrides::read(table, true);
    match overrides.problem() {
        Some(message) => Err(GlobalConfigError::Defaults(message.to_owned())),
        None => Ok(overrides),
    }
}

fn write_or_remove(
    table: &mut dyn TableLike,
    def: &settings::KeyDef,
    value: Option<&settings::Value>,
) {
    match value {
        Some(value) => replace_value(table, def.short(), value.to_toml()),
        None => {
            table.remove(def.short());
        }
    }
}

/// The path `config.toml` really is: a symlink (GNU stow) is followed, so the
/// link survives writes.
async fn real_config_file(paths: &Paths) -> Result<PathBuf, ConfigSaveError> {
    let file = paths.config_file();
    if let Ok(target) = tokio::fs::canonicalize(&file).await {
        Ok(target)
    } else if let Ok(meta) = tokio::fs::symlink_metadata(&file).await
        && meta.is_symlink()
    {
        let target = tokio::fs::read_link(&file).await.unwrap_or(file);
        Err(ConfigSaveError::DanglingLink { target })
    } else {
        Ok(file)
    }
}

fn reject_unknown_keys(document: &DocumentMut, version: i64) -> Result<(), GlobalConfigError> {
    let has_behavior_keys = version != FIRST_VERSION;
    let top_level: &[&str] = if has_behavior_keys {
        &TOP_LEVEL_KEYS
    } else {
        &V1_TOP_LEVEL_KEYS
    };
    let unknown = |table: &dyn TableLike, allowed: &dyn Fn(&str) -> bool, prefix: &str| {
        table
            .iter()
            .map(|(key, _)| key)
            .find(|key| !allowed(key))
            .map(|key| GlobalConfigError::UnknownKey(format!("{prefix}{key}")))
    };
    if let Some(err) = unknown(document.as_table(), &|key| top_level.contains(&key), "") {
        return Err(err);
    }
    if let Some(table) = document.get("defaults").and_then(Item::as_table_like)
        && let Some(err) = unknown(table, &|key| settings::key(key).is_some(), "defaults.")
    {
        return Err(err);
    }
    for section in APP_TABLES {
        if let Some(table) = document.get(section).and_then(Item::as_table_like)
            && let Some(err) = unknown(
                table,
                &|key| settings::app_key(&format!("{section}.{key}")).is_some(),
                &format!("{section}."),
            )
        {
            return Err(err);
        }
    }
    let pair_key =
        |key: &str| PAIR_KEYS.contains(&key) || (has_behavior_keys && settings::key(key).is_some());
    if let Some(tables) = document.get("pair").and_then(Item::as_array_of_tables) {
        for table in tables {
            if let Some(err) = unknown(table, &pair_key, "") {
                return Err(err);
            }
        }
    }
    Ok(())
}

/// `path` with a leading `~` replaced by `home`.
#[must_use]
pub fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => home.join(rest),
        Err(_) => path.to_owned(),
    }
}

/// `syncfast-watch.service` already uses this name; a sync with it would
/// share the unit file.
pub const RESERVED_ID: &str = "watch";

/// The id a sync named `name` gets: lowercase, every run of characters
/// outside `a-z0-9` becomes one `-`, cut to 32. Without a letter or digit
/// the id is `sync`.
#[must_use]
pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = slug.get(..slug.len().min(MAX_ID_LEN)).unwrap_or(slug);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "sync".to_owned()
    } else {
        slug.to_owned()
    }
}

/// [`slugify`] of `name`, with `-2`, `-3`... added until no id in `taken`
/// (and not the reserved one) is the same. The result stays within 32
/// characters.
#[must_use]
pub fn unique_id<'a>(name: &str, taken: impl IntoIterator<Item = &'a str>) -> String {
    let taken: Vec<&str> = taken.into_iter().collect();
    let base = slugify(name);
    let is_free = |id: &str| id != RESERVED_ID && !taken.contains(&id);
    if is_free(&base) {
        return base;
    }
    let mut number = 2_u32;
    loop {
        let suffix = format!("-{number}");
        let room = MAX_ID_LEN.saturating_sub(suffix.len());
        let head = base.get(..base.len().min(room)).unwrap_or(&base);
        let id = format!("{}{suffix}", head.trim_end_matches('-'));
        if is_free(&id) {
            return id;
        }
        number += 1;
    }
}

const MAX_ID_LEN: usize = 32;

/// True for ids that name a sync: `a-z`, `0-9` and `-`, up to 32 characters.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 32
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Checks that run before the cross-pair overlap checks, in spec order.
fn problem_before_overlap(
    pair: &Pair,
    duplicate_id: bool,
    data_dir: &Path,
    defaults: &Behavior,
) -> Option<String> {
    if !valid_id(&pair.id) {
        return Some(format!(
            "sync id \"{}\" must use a-z, 0-9 and \"-\", up to 32 characters",
            pair.id
        ));
    }
    if pair.id == RESERVED_ID {
        return Some(format!("sync id \"{}\" is reserved", pair.id));
    }
    if duplicate_id {
        return Some(format!("duplicate sync id \"{}\"", pair.id));
    }
    if pair.name.trim().is_empty() || pair.name.chars().any(char::is_control) {
        return Some("sync name must be one line of text".to_owned());
    }
    if !pair.local.is_absolute() {
        return Some("local folder must be an absolute path".to_owned());
    }
    if data_dir.starts_with(resolve(&pair.local)) {
        return Some("local folder cannot contain syncfast's versions folder".to_owned());
    }
    if split_remote(&pair.remote).is_none() {
        return Some("remote folder must look like \"account:folder\"".to_owned());
    }
    if let Some(problem) = pair.overrides.problem() {
        return Some(problem.to_owned());
    }
    settings::relation_problem(&Behavior::for_pair(defaults, pair))
}

/// Checks that run after the overlap checks, in spec order.
fn problem_after_overlap(pair: &Pair, accounts: &HashSet<&str>) -> Option<String> {
    if let Some((account, _)) = split_remote(&pair.remote)
        && !accounts.contains(account)
    {
        return Some(format!("account \"{account}\" is not set up in rclone"));
    }
    for (index, line) in pair.filters.iter().enumerate() {
        if let Some(problem) = filter_line_problem(index + 1, line) {
            return Some(problem);
        }
    }
    pair.presets
        .iter()
        .find(|name| filters::preset(name).is_none())
        .map(|name| format!("unknown preset \"{name}\""))
}

/// What is wrong with the custom filter line `number` (counting from 1).
#[must_use]
pub fn filter_line_problem(number: usize, line: &str) -> Option<String> {
    if !(line.starts_with("+ ") || line.starts_with("- ")) {
        return Some(format!(
            "filter line {number} must start with \"+ \" or \"- \""
        ));
    }
    if line.contains(['\n', '\r']) {
        return Some(format!("filter line {number} must be a single line"));
    }
    None
}

/// True when two `account:folder` remotes cover the same files: same
/// account and one folder is the other or inside it (the root covers all).
#[must_use]
pub fn remotes_overlap(a: &str, b: &str) -> bool {
    let (Some((account_a, path_a)), Some((account_b, path_b))) = (split_remote(a), split_remote(b))
    else {
        return false;
    };
    if account_a != account_b {
        return false;
    }
    let (a, b) = (segments(path_a), segments(path_b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// True when `inner` is `outer` or inside it, on the same account.
#[must_use]
pub fn remote_within(inner: &str, outer: &str) -> bool {
    let (Some((account_inner, path_inner)), Some((account_outer, path_outer))) =
        (split_remote(inner), split_remote(outer))
    else {
        return false;
    };
    account_inner == account_outer && segments(path_inner).starts_with(&segments(path_outer))
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Canonicalizes the longest existing ancestor and re-appends the rest, so a
/// folder that is not there yet (or sits behind a symlink) still compares
/// correctly.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    let cleaned = lexical_clean(path);
    let mut missing: Vec<&OsStr> = Vec::new();
    let mut cursor = cleaned.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(cursor) {
            return missing.iter().rev().fold(real, |acc, name| acc.join(name));
        }
        match (cursor.parent(), cursor.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                cursor = parent;
            }
            _ => return cleaned.clone(),
        }
    }
}

pub(crate) fn lexical_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const HOME: &str = "/home/ada";

    fn paths_in(root: &Path) -> Paths {
        Paths::new(
            PathBuf::from(HOME),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    fn pair_block(id: &str, name: &str, local: &str, remote: &str) -> String {
        format!(
            "[[pair]]\nid = \"{id}\"\nname = \"{name}\"\nlocal = \"{local}\"\nremote = \"{remote}\"\n"
        )
    }

    fn config(blocks: &[String]) -> Config {
        let text = format!("version = 2\n\n{}", blocks.join("\n"));
        Config::parse(&text, Path::new(HOME)).unwrap()
    }

    fn accounts() -> Vec<String> {
        vec!["gdrive:".into(), "other:".into()]
    }

    fn validate(config: &Config) -> Validation {
        let root = tempfile::tempdir().unwrap();
        config.validate(&paths_in(root.path()), &accounts())
    }

    fn messages(config: &Config) -> Vec<String> {
        validate(config)
            .errors
            .into_iter()
            .map(|err| err.message)
            .collect()
    }

    fn single_problem(extra_keys: &str) -> Vec<String> {
        let block = format!(
            "{}{extra_keys}",
            pair_block("drive", "Drive", "/data/Drive", "gdrive:")
        );
        messages(&config(&[block]))
    }

    #[test]
    fn parse_reads_a_full_pair_and_applies_defaults_to_omitted_keys() {
        let config = config(&[pair_block(
            "drive",
            "Google Drive",
            "/data/Drive",
            "gdrive:",
        )]);

        let pair = &config.pairs()[0];
        assert_eq!(pair.id, "drive");
        assert_eq!(pair.name, "Google Drive");
        assert_eq!(pair.local, Path::new("/data/Drive"));
        assert_eq!(pair.remote, "gdrive:");
        assert!(pair.enabled);
        assert_eq!(pair.presets, ["system_junk", "dev_dirs", "git_repos"]);
        assert!(pair.filters.is_empty());
    }

    #[test]
    fn parse_expands_a_leading_tilde_with_home() {
        let config = config(&[
            pair_block("a", "A", "~/Drive", "gdrive:"),
            pair_block("b", "B", "~", "other:"),
            pair_block("c", "C", "~ada/x", "other:x"),
        ]);

        assert_eq!(config.pairs()[0].local, Path::new("/home/ada/Drive"));
        assert_eq!(config.pairs()[1].local, Path::new("/home/ada"));
        assert_eq!(config.pairs()[2].local, Path::new("~ada/x"));
    }

    #[test]
    fn parse_accepts_pairs_written_as_an_inline_array() {
        let text = "version = 1\npair = [{ id = \"drive\", name = \"D\", local = \"/d\", remote = \"gdrive:\" }]\n";

        let mut config = Config::parse(text, Path::new(HOME)).unwrap();
        assert!(config.set_enabled("drive", false));

        assert_eq!(config.pairs()[0].id, "drive");
        assert!(config.to_toml().contains("enabled = false"));
    }

    #[test]
    fn invalid_toml_is_a_global_error_prefixed_with_the_file_name() {
        let err = Config::parse("version = [", Path::new(HOME)).unwrap_err();

        assert!(err.to_string().starts_with("config.toml: "), "{err}");
    }

    #[test]
    fn wrong_value_type_is_a_global_error_prefixed_with_the_file_name() {
        let text = "version = 1\n[[pair]]\nid = \"a\"\nname = \"A\"\nlocal = \"/a\"\nremote = \"gdrive:\"\nenabled = \"yes\"\n";

        let err = Config::parse(text, Path::new(HOME)).unwrap_err();

        assert!(err.to_string().starts_with("config.toml: "), "{err}");
    }

    #[test]
    fn unknown_top_level_key_is_rejected_by_name() {
        let err = Config::parse("version = 1\ntheme = \"x\"\n", Path::new(HOME)).unwrap_err();

        assert_eq!(
            err.to_string(),
            "unknown key \"theme\": not supported by this syncfast version"
        );
    }

    #[test]
    fn unknown_pair_key_is_rejected_by_name() {
        let text = format!(
            "version = 1\n{}review = true\n",
            pair_block("a", "A", "/a", "gdrive:")
        );

        let err = Config::parse(&text, Path::new(HOME)).unwrap_err();

        assert_eq!(
            err.to_string(),
            "unknown key \"review\": not supported by this syncfast version"
        );
    }

    #[test]
    fn version_3_is_unsupported() {
        let err = Config::parse("version = 3\nfuture = true\n", Path::new(HOME)).unwrap_err();

        assert_eq!(err.to_string(), "unsupported config version 3");
    }

    #[test]
    fn missing_version_is_a_global_error() {
        let err = Config::parse("", Path::new(HOME)).unwrap_err();

        assert!(err.to_string().starts_with("config.toml: "), "{err}");
    }

    #[tokio::test]
    async fn load_of_an_absent_file_is_an_empty_config() {
        let root = tempfile::tempdir().unwrap();

        let config = Config::load(&paths_in(root.path())).await.unwrap();

        assert!(config.pairs().is_empty());
        assert_eq!(config.to_toml(), "version = 2\n");
    }

    #[tokio::test]
    async fn load_reads_the_file_under_the_config_dir() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let block = pair_block("drive", "Drive", "~/Drive", "gdrive:");
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.config_file(), format!("version = 1\n{block}")).unwrap();

        let config = Config::load(&paths).await.unwrap();

        assert_eq!(config.pairs()[0].local, Path::new("/home/ada/Drive"));
    }

    #[test]
    fn a_valid_pair_has_no_error_and_is_returned() {
        let config = config(&[pair_block("drive", "Drive", "/data/Drive", "gdrive:Notes")]);

        let validation = validate(&config);

        assert!(validation.errors.is_empty());
        assert_eq!(validation.pairs, config.pairs());
    }

    #[test]
    fn id_outside_the_allowed_pattern_is_rejected() {
        for id in ["", "Drive", "-drive", "my drive", "dri_ve", &"a".repeat(33)] {
            let config = config(&[pair_block(id, "Drive", "/data/Drive", "gdrive:")]);

            assert_eq!(
                messages(&config),
                [format!(
                    "sync id \"{id}\" must use a-z, 0-9 and \"-\", up to 32 characters"
                )]
            );
        }
    }

    #[test]
    fn id_watch_is_reserved_for_the_watcher_unit() {
        let config = config(&[pair_block("watch", "Watch", "/data/Watch", "gdrive:")]);

        assert_eq!(messages(&config), ["sync id \"watch\" is reserved"]);
    }

    #[test]
    fn id_of_exactly_32_allowed_characters_is_accepted() {
        let id = format!("a{}", "-0".repeat(15) + "z");
        assert_eq!(id.len(), 32);

        let config = config(&[pair_block(&id, "Drive", "/data/Drive", "gdrive:")]);

        assert!(messages(&config).is_empty());
    }

    #[test]
    fn duplicate_ids_invalidate_both_pairs_and_leave_the_others_valid() {
        let config = config(&[
            pair_block("same", "One", "/data/one", "gdrive:one"),
            pair_block("same", "Two", "/data/two", "gdrive:two"),
            pair_block("fine", "Three", "/data/three", "gdrive:three"),
        ]);

        let validation = validate(&config);

        assert_eq!(
            validation.errors,
            [
                PairError {
                    pair_id: "same".into(),
                    message: "duplicate sync id \"same\"".into()
                },
                PairError {
                    pair_id: "same".into(),
                    message: "duplicate sync id \"same\"".into()
                },
            ]
        );
        assert_eq!(validation.pairs.len(), 1);
        assert_eq!(validation.pairs[0].id, "fine");
    }

    #[test]
    fn empty_or_multiline_name_is_rejected() {
        for name in ["", "   ", "two\\nlines", "tab\\there"] {
            let config = config(&[pair_block("drive", name, "/data/Drive", "gdrive:")]);

            assert_eq!(messages(&config), ["sync name must be one line of text"]);
        }
    }

    #[test]
    fn relative_local_folder_is_rejected() {
        let config = config(&[pair_block("drive", "Drive", "Documents/Drive", "gdrive:")]);

        assert_eq!(messages(&config), ["local folder must be an absolute path"]);
    }

    #[test]
    fn local_folder_containing_the_versions_folder_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        for local in [
            root.path().join("data"),
            paths.data_dir(),
            PathBuf::from("/"),
        ] {
            let config = config(&[pair_block(
                "drive",
                "Drive",
                local.to_str().unwrap(),
                "gdrive:",
            )]);

            let validation = config.validate(&paths, &accounts());

            assert_eq!(
                validation.errors[0].message,
                "local folder cannot contain syncfast's versions folder",
                "{}",
                local.display()
            );
        }
    }

    #[test]
    fn local_folder_beside_the_versions_folder_is_accepted() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let local = root.path().join("data").join("syncfast-notes");
        let config = config(&[pair_block(
            "drive",
            "Drive",
            local.to_str().unwrap(),
            "gdrive:",
        )]);

        assert!(config.validate(&paths, &accounts()).errors.is_empty());
    }

    #[test]
    fn local_folders_that_are_equal_or_nested_overlap_in_both_directions() {
        let config = config(&[
            pair_block("a", "Alpha", "/data/Docs", "gdrive:a"),
            pair_block("b", "Beta", "/data/Docs/Sub", "gdrive:b"),
            pair_block("c", "Gamma", "/data/Docs", "gdrive:c"),
        ]);

        assert_eq!(
            messages(&config),
            [
                "local folder overlaps \"Beta\"",
                "local folder overlaps \"Alpha\"",
                "local folder overlaps \"Alpha\"",
            ]
        );
    }

    #[test]
    fn local_folders_sharing_only_a_name_prefix_do_not_overlap() {
        let config = config(&[
            pair_block("a", "Alpha", "/data/Docs", "gdrive:a"),
            pair_block("b", "Beta", "/data/Docs2", "gdrive:b"),
        ]);

        assert!(messages(&config).is_empty());
    }

    #[test]
    fn local_overlap_follows_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let config = config(&[
            pair_block("a", "Alpha", real.to_str().unwrap(), "gdrive:a"),
            pair_block(
                "b",
                "Beta",
                link.join("deeper").to_str().unwrap(),
                "gdrive:b",
            ),
        ]);

        let validation = config.validate(&paths_in(root.path()), &accounts());

        assert_eq!(validation.errors.len(), 2);
        assert_eq!(
            validation.errors[0].message,
            "local folder overlaps \"Beta\""
        );
    }

    #[test]
    fn remote_root_overlaps_every_folder_of_the_same_account() {
        let config = config(&[
            pair_block("a", "Alpha", "/data/a", "gdrive:"),
            pair_block("b", "Beta", "/data/b", "gdrive:Notes/2024"),
            pair_block("c", "Gamma", "/data/c", "other:Notes"),
        ]);

        assert_eq!(
            messages(&config),
            [
                "remote folder overlaps \"Beta\"",
                "remote folder overlaps \"Alpha\"",
            ]
        );
    }

    #[test]
    fn remote_folders_overlap_by_path_component_not_by_text_prefix() {
        let config = config(&[
            pair_block("a", "Alpha", "/data/a", "gdrive:Notes"),
            pair_block("b", "Beta", "/data/b", "gdrive:Notes2"),
            pair_block("c", "Gamma", "/data/c", "gdrive:Notes/"),
        ]);

        assert_eq!(
            messages(&config),
            [
                "remote folder overlaps \"Gamma\"",
                "remote folder overlaps \"Alpha\"",
            ]
        );
    }

    #[test]
    fn a_pair_that_is_invalid_on_its_own_does_not_cause_overlap_errors_in_others() {
        let config = config(&[
            pair_block("a", "Alpha", "/data/Docs", "gdrive:a"),
            pair_block("b", "Beta", "relative/Docs", "gdrive:a"),
        ]);

        let validation = validate(&config);

        assert_eq!(validation.pairs.len(), 1);
        assert_eq!(validation.errors[0].pair_id, "b");
    }

    #[test]
    fn account_missing_from_the_rclone_remotes_is_rejected() {
        let config = config(&[pair_block("drive", "Drive", "/data/Drive", "gone:Notes")]);

        assert_eq!(
            messages(&config),
            ["account \"gone\" is not set up in rclone"]
        );
    }

    #[test]
    fn overlap_is_reported_before_an_unknown_account() {
        let config = config(&[
            pair_block("a", "Alpha", "/data/Docs", "gone:a"),
            pair_block("b", "Beta", "/data/Docs/Sub", "gdrive:b"),
            pair_block("c", "Gamma", "/data/c", "gone:Notes"),
            pair_block("d", "Delta", "/data/d", "gone:Notes/2024"),
        ]);

        assert_eq!(
            messages(&config),
            [
                "local folder overlaps \"Beta\"",
                "local folder overlaps \"Alpha\"",
                "remote folder overlaps \"Delta\"",
                "remote folder overlaps \"Gamma\"",
            ]
        );
    }

    #[test]
    fn local_overlap_is_reported_before_remote_overlap_whatever_the_pair_order() {
        let config = config(&[
            pair_block("a", "Alpha", "/data/a", "gdrive:Notes"),
            pair_block("b", "Beta", "/data/b", "gdrive:Notes"),
            pair_block("c", "Gamma", "/data/a", "other:c"),
        ]);

        assert_eq!(
            messages(&config),
            [
                "local folder overlaps \"Gamma\"",
                "remote folder overlaps \"Alpha\"",
                "local folder overlaps \"Alpha\"",
            ]
        );
    }

    #[test]
    fn account_names_without_a_trailing_colon_are_accepted() {
        let root = tempfile::tempdir().unwrap();
        let config = config(&[pair_block("drive", "Drive", "/data/Drive", "gdrive:")]);

        let validation = config.validate(&paths_in(root.path()), &["gdrive".to_owned()]);

        assert!(validation.errors.is_empty());
    }

    #[test]
    fn remote_without_an_account_is_rejected() {
        let config = config(&[pair_block("drive", "Drive", "/data/Drive", "Notes")]);

        assert_eq!(
            messages(&config),
            ["remote folder must look like \"account:folder\""]
        );
    }

    #[test]
    fn filter_line_without_a_sign_is_rejected_with_its_one_based_number() {
        assert_eq!(
            single_problem("filters = [\"- a\", \"b\"]\n"),
            ["filter line 2 must start with \"+ \" or \"- \""]
        );
        assert_eq!(
            single_problem("filters = [\"-a\"]\n"),
            ["filter line 1 must start with \"+ \" or \"- \""]
        );
    }

    #[test]
    fn filter_line_with_an_embedded_newline_is_rejected() {
        assert_eq!(
            single_problem("filters = [\"- a\\n+ b\"]\n"),
            ["filter line 1 must be a single line"]
        );
    }

    #[test]
    fn unknown_preset_is_rejected() {
        assert_eq!(
            single_problem("presets = [\"dev_dirs\", \"nope\"]\n"),
            ["unknown preset \"nope\""]
        );
    }

    #[test]
    fn well_formed_filters_and_known_presets_are_accepted() {
        assert!(
            single_problem("filters = [\"+ keep/**\", \"- *.bak\"]\npresets = [\"dev_dirs\"]\n")
                .is_empty()
        );
    }

    #[test]
    fn save_keeps_comments_and_key_order_across_edits() {
        let text = "# my syncs\nversion = 1\n\n# the main one\n[[pair]]\nid = \"drive\" # short id\nname = \"Drive\"\nlocal = \"/data/Drive\"\nremote = \"gdrive:\"\n# pause with false\nenabled = true\npresets = [\"system_junk\", \"dev_dirs\"] # defaults\nfilters = []\n";
        let mut config = Config::parse(text, Path::new(HOME)).unwrap();

        config.set_enabled("drive", false);
        config.set_filters("drive", &["dev_dirs".to_owned()], &["- *.bak".to_owned()]);

        assert_eq!(
            config.to_toml(),
            "# my syncs\nversion = 1\n\n# the main one\n[[pair]]\nid = \"drive\" # short id\nname = \"Drive\"\nlocal = \"/data/Drive\"\nremote = \"gdrive:\"\n# pause with false\nenabled = false\npresets = [\"dev_dirs\"] # defaults\nfilters = [\"- *.bak\"]\n"
        );
    }

    #[test]
    fn rename_account_moves_only_its_syncs_and_keeps_comments() {
        let text = "version = 1\n\n# the notes\n[[pair]]\nid = \"notes\"\nname = \"Notes\"\nlocal = \"/data/Notes\"\nremote = \"gdrive:Notes\" # on drive\n\n[[pair]]\nid = \"all\"\nname = \"All\"\nlocal = \"/data/All\"\nremote = \"gdrive:\"\n\n[[pair]]\nid = \"elsewhere\"\nname = \"Elsewhere\"\nlocal = \"/data/Elsewhere\"\nremote = \"other:x\"\n";
        let mut config = Config::parse(text, Path::new(HOME)).unwrap();

        let moved = config.rename_account("gdrive", "personal");

        assert_eq!(moved, ["notes", "all"]);
        assert_eq!(
            config.to_toml(),
            "version = 1\n\n# the notes\n[[pair]]\nid = \"notes\"\nname = \"Notes\"\nlocal = \"/data/Notes\"\nremote = \"personal:Notes\" # on drive\n\n[[pair]]\nid = \"all\"\nname = \"All\"\nlocal = \"/data/All\"\nremote = \"personal:\"\n\n[[pair]]\nid = \"elsewhere\"\nname = \"Elsewhere\"\nlocal = \"/data/Elsewhere\"\nremote = \"other:x\"\n"
        );
        let remotes: Vec<_> = config
            .pairs()
            .iter()
            .map(|pair| pair.remote.as_str())
            .collect();
        assert_eq!(remotes, ["personal:Notes", "personal:", "other:x"]);
    }

    #[test]
    fn edits_are_visible_through_pairs() {
        let mut config = config(&[pair_block("drive", "Drive", "/data/Drive", "gdrive:")]);

        config.set_enabled("drive", false);
        config.set_filters("drive", &[], &["+ a".to_owned()]);

        let pair = &config.pairs()[0];
        assert!(!pair.enabled);
        assert!(pair.presets.is_empty());
        assert_eq!(pair.filters, ["+ a"]);
    }

    #[test]
    fn set_enabled_and_set_filters_report_whether_the_pair_exists() {
        let mut config = config(&[pair_block("drive", "Drive", "/data/Drive", "gdrive:")]);

        assert!(!config.set_enabled("ghost", false));
        assert!(!config.set_filters("ghost", &[], &[]));
        assert!(config.set_enabled("drive", true));
    }

    #[test]
    fn set_enabled_adds_the_key_when_the_pair_omitted_it() {
        let mut config = config(&[pair_block("drive", "Drive", "/data/Drive", "gdrive:")]);

        config.set_enabled("drive", false);

        let reparsed = Config::parse(&config.to_toml(), Path::new(HOME)).unwrap();
        assert!(!reparsed.pairs()[0].enabled);
    }

    #[test]
    fn add_pair_appends_a_table_that_reloads_identically() {
        let mut config = config(&[pair_block("drive", "Drive", "/data/Drive", "gdrive:")]);
        let added = Pair {
            id: "notes".into(),
            name: "Notes".into(),
            local: PathBuf::from("/data/Notes"),
            remote: "gdrive:Notes".into(),
            enabled: false,
            presets: vec!["dev_dirs".into()],
            filters: vec!["- *.bak".into()],
            overrides: Overrides::default(),
        };

        config.add_pair(added.clone()).unwrap();

        let reparsed = Config::parse(&config.to_toml(), Path::new(HOME)).unwrap();
        assert_eq!(reparsed.pairs(), config.pairs());
        assert_eq!(reparsed.pairs()[1], added);
        assert_eq!(reparsed.pairs()[0].id, "drive");
    }

    #[test]
    fn add_pair_to_an_empty_config_creates_the_first_table() {
        let mut config = Config::empty();

        config
            .add_pair(Pair {
                id: "a".into(),
                name: "A".into(),
                local: PathBuf::from("/a"),
                remote: "gdrive:".into(),
                enabled: true,
                presets: vec![],
                filters: vec![],
                overrides: Overrides::default(),
            })
            .unwrap();

        let reparsed = Config::parse(&config.to_toml(), Path::new(HOME)).unwrap();
        assert_eq!(reparsed.pairs().len(), 1);
    }

    #[test]
    fn remove_pair_drops_only_that_table_and_keeps_the_comments_of_the_rest() {
        let text = "version = 1\n\n[[pair]]\nid = \"a\"\nname = \"A\"\nlocal = \"/a\"\nremote = \"gdrive:a\"\n\n# keep me\n[[pair]]\nid = \"b\"\nname = \"B\"\nlocal = \"/b\"\nremote = \"gdrive:b\"\n";
        let mut config = Config::parse(text, Path::new(HOME)).unwrap();

        assert!(config.remove_pair("a"));

        assert!(config.to_toml().contains("# keep me"));
        assert!(!config.to_toml().contains("id = \"a\""));
        assert_eq!(config.pairs().len(), 1);
        assert_eq!(config.pairs()[0].id, "b");
        assert!(!config.remove_pair("a"));
    }

    #[test]
    fn removing_the_last_pair_leaves_a_loadable_config() {
        let mut config = config(&[pair_block("a", "A", "/a", "gdrive:")]);

        config.remove_pair("a");

        let reparsed = Config::parse(&config.to_toml(), Path::new(HOME)).unwrap();
        assert!(reparsed.pairs().is_empty());
    }

    #[tokio::test]
    async fn save_writes_the_file_and_load_reads_it_back() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let mut config = Config::empty();
        config
            .add_pair(Pair {
                id: "a".into(),
                name: "A".into(),
                local: PathBuf::from("/a"),
                remote: "gdrive:".into(),
                enabled: true,
                presets: vec![],
                filters: vec![],
                overrides: Overrides::default(),
            })
            .unwrap();

        config.save(&paths).await.unwrap();
        let loaded = Config::load(&paths).await.unwrap();

        assert_eq!(loaded.pairs(), config.pairs());
    }

    #[tokio::test]
    async fn save_through_a_symlink_updates_the_target_and_keeps_the_link() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let dotfiles = root.path().join("dotfiles");
        std::fs::create_dir_all(&dotfiles).unwrap();
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        let target = dotfiles.join("config.toml");
        std::fs::write(&target, "# stowed\nversion = 1\n").unwrap();
        std::os::unix::fs::symlink(&target, paths.config_file()).unwrap();
        let mut config = Config::load(&paths).await.unwrap();
        config
            .add_pair(Pair {
                id: "a".into(),
                name: "A".into(),
                local: PathBuf::from("/a"),
                remote: "gdrive:".into(),
                enabled: true,
                presets: vec![],
                filters: vec![],
                overrides: Overrides::default(),
            })
            .unwrap();

        config.save(&paths).await.unwrap();

        assert!(
            std::fs::symlink_metadata(paths.config_file())
                .unwrap()
                .is_symlink()
        );
        let stowed = std::fs::read_to_string(&target).unwrap();
        assert!(stowed.starts_with("# stowed\nversion = 2\n"));
        assert!(stowed.contains("id = \"a\""));
    }

    fn sample_pair(id: &str) -> Pair {
        Pair {
            id: id.into(),
            name: id.to_uppercase(),
            local: PathBuf::from(format!("/{id}")),
            remote: "gdrive:".into(),
            enabled: true,
            presets: vec![],
            filters: vec![],
            overrides: Overrides::default(),
        }
    }

    #[tokio::test]
    async fn concurrent_edits_of_different_pairs_both_persist() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let mut config = Config::empty();
        config.add_pair(sample_pair("a")).unwrap();
        config.add_pair(sample_pair("b")).unwrap();
        config.save(&paths).await.unwrap();

        let (first, second) = tokio::join!(
            Config::edit(&paths, |config| config.set_enabled("a", false)),
            Config::edit(&paths, |config| config.set_enabled("b", false)),
        );

        assert!(first.unwrap() && second.unwrap());
        let loaded = Config::load(&paths).await.unwrap();
        assert!(loaded.pairs().iter().all(|pair| !pair.enabled));
    }

    #[tokio::test]
    async fn an_edit_keeps_its_lock_out_of_the_config_folder() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let mut config = Config::empty();
        config.add_pair(sample_pair("a")).unwrap();
        config.save(&paths).await.unwrap();

        Config::edit(&paths, |config| config.set_enabled("a", false))
            .await
            .unwrap();

        assert!(paths.state_dir().join("config.lock").exists());
        assert!(!paths.config_dir().join("config.lock").exists());
    }

    #[tokio::test]
    async fn an_edit_that_changes_nothing_does_not_write_the_file() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());

        let found = Config::edit(&paths, |config| config.remove_pair("missing"))
            .await
            .unwrap();

        assert!(!found);
        assert!(!paths.config_file().exists());
    }

    #[tokio::test]
    async fn an_edit_of_a_broken_config_reports_the_load_error_and_keeps_the_file() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        std::fs::write(paths.config_file(), "version = 9\n").unwrap();

        let err = Config::edit(&paths, |config| config.set_enabled("a", false))
            .await
            .unwrap_err();

        assert!(matches!(
            err,
            ConfigUpdateError::Load(GlobalConfigError::UnsupportedVersion(9))
        ));
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            "version = 9\n"
        );
    }

    #[tokio::test]
    async fn save_refuses_a_link_to_a_missing_file_and_keeps_the_link() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        std::fs::create_dir_all(paths.config_dir()).unwrap();
        let missing = root.path().join("dotfiles/config.toml");
        std::os::unix::fs::symlink(&missing, paths.config_file()).unwrap();

        let err = Config::empty().save(&paths).await.unwrap_err();

        assert_eq!(
            err.to_string(),
            format!(
                "config.toml is a link to a missing file: {}",
                missing.display()
            )
        );
        assert_eq!(std::fs::read_link(paths.config_file()).unwrap(), missing);
    }

    #[test]
    fn slug_lowercases_and_turns_each_run_of_other_characters_into_one_dash() {
        assert_eq!(slugify("Google Drive"), "google-drive");
        assert_eq!(slugify("  Meu  Drive!! "), "meu-drive");
        assert_eq!(slugify("Notas/2026_final"), "notas-2026-final");
    }

    #[test]
    fn slug_without_letters_or_digits_is_sync() {
        assert_eq!(slugify("***"), "sync");
        assert_eq!(slugify(""), "sync");
    }

    #[test]
    fn slug_is_cut_to_32_characters_without_a_trailing_dash() {
        let long = "a".repeat(31) + " b";

        let slug = slugify(&long);

        assert_eq!(slug, "a".repeat(31));
        assert!(valid_id(&slugify(&"x".repeat(80))));
    }

    #[test]
    fn unique_id_adds_the_first_free_number_after_a_collision() {
        assert_eq!(unique_id("Notes", ["other"]), "notes");
        assert_eq!(unique_id("Notes", ["notes"]), "notes-2");
        assert_eq!(unique_id("Notes", ["notes", "notes-2"]), "notes-3");
    }

    #[test]
    fn unique_id_keeps_within_32_characters_when_it_adds_a_number() {
        let name = "a".repeat(40);
        let full = "a".repeat(32);

        let id = unique_id(&name, [full.as_str()]);

        assert_eq!(id, format!("{}-2", "a".repeat(30)));
        assert!(valid_id(&id));
    }

    #[test]
    fn unique_id_never_uses_the_reserved_name() {
        assert_eq!(unique_id("watch", []), "watch-2");
    }

    #[test]
    fn filter_line_problem_names_the_line_and_the_rule_it_breaks() {
        assert_eq!(filter_line_problem(1, "- a.tmp"), None);
        assert_eq!(filter_line_problem(2, "+ keep/**"), None);
        assert_eq!(
            filter_line_problem(3, "a.tmp").as_deref(),
            Some("filter line 3 must start with \"+ \" or \"- \"")
        );
    }

    const V1_FILE: &str = "# mine\nversion = 1\n\n[[pair]]\nid = \"a\"\nname = \"A\"\nlocal = \"/a\"\nremote = \"gdrive:a\"\n";

    fn write_config_file(paths: &Paths, text: &str) {
        let file = paths.config_file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn parse(text: &str) -> Config {
        Config::parse(text, Path::new(HOME)).unwrap()
    }

    fn global_error(text: &str) -> String {
        Config::parse(text, Path::new(HOME))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn a_v1_config_loads_and_reports_it_was_migrated_from_1() {
        let old = parse(V1_FILE);
        let current = parse("version = 2\n");

        assert_eq!(old.migrated_from(), Some(1));
        assert_eq!(old.pairs()[0].id, "a");
        assert_eq!(current.migrated_from(), None);
    }

    #[tokio::test]
    async fn the_first_save_of_a_v1_config_writes_version_2_and_keeps_comments() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        write_config_file(&paths, V1_FILE);

        Config::edit(&paths, |config| config.set_enabled("a", false))
            .await
            .unwrap();

        let saved = std::fs::read_to_string(paths.config_file()).unwrap();
        assert!(saved.starts_with("# mine\nversion = 2\n"), "{saved}");
        assert!(saved.contains("enabled = false"), "{saved}");
        assert_eq!(Config::load(&paths).await.unwrap().migrated_from(), None);
    }

    #[tokio::test]
    async fn an_edit_that_changes_nothing_leaves_a_v1_file_as_it_was() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        write_config_file(&paths, V1_FILE);

        Config::edit(&paths, |_| ()).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            V1_FILE
        );
    }

    #[tokio::test]
    async fn migrate_copies_a_v1_file_to_config_toml_v1_once_and_leaves_a_v2_file_alone() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());
        let backup = paths.config_file().with_file_name("config.toml.v1");
        write_config_file(&paths, V1_FILE);

        let first = Config::migrate(&paths).await.unwrap();
        let migrated = std::fs::read_to_string(paths.config_file()).unwrap();
        let second = Config::migrate(&paths).await.unwrap();
        write_config_file(&paths, "version = 1\n# edited again\n");
        let third = Config::migrate(&paths).await.unwrap();

        assert!(first);
        assert!(migrated.starts_with("# mine\nversion = 2\n"), "{migrated}");
        assert!(!second);
        assert!(third);
        assert_eq!(std::fs::read_to_string(backup).unwrap(), V1_FILE);
    }

    #[tokio::test]
    async fn migrate_does_nothing_without_a_config_file() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path());

        let migrated = Config::migrate(&paths).await.unwrap();

        assert!(!migrated);
        assert!(!paths.config_file().exists());
    }

    #[test]
    fn a_v1_config_with_a_defaults_table_is_an_unknown_key_error() {
        assert_eq!(
            global_error("version = 1\n[defaults]\ndebounce = \"10s\"\n"),
            "unknown key \"defaults\": not supported by this syncfast version"
        );
    }

    #[test]
    fn a_v1_pair_with_a_behavior_key_is_an_unknown_key_error() {
        assert_eq!(
            global_error(&format!(
                "version = 1\n{}debounce = \"10s\"\n",
                pair_block("a", "A", "/a", "gdrive:")
            )),
            "unknown key \"debounce\": not supported by this syncfast version"
        );
    }

    #[test]
    fn an_unknown_key_in_defaults_is_named_with_its_table() {
        assert_eq!(
            global_error("version = 2\n[defaults]\nfuture = 1\n"),
            "unknown key \"defaults.future\": not supported by this syncfast version"
        );
    }

    #[test]
    fn an_invalid_defaults_value_is_a_global_error_with_the_key_message() {
        assert_eq!(
            global_error("version = 2\n[defaults]\ndebounce = \"2s\"\n"),
            "config.toml: [defaults]: debounce must be at least 5s"
        );
        assert_eq!(
            global_error("version = 2\n[defaults]\nwatch = \"yes\"\n"),
            "config.toml: [defaults]: watch must be bool"
        );
    }

    #[test]
    fn a_defaults_table_reads_into_the_defaults_behavior() {
        let config = parse("version = 2\n[defaults]\ndebounce = \"10s\"\nmax_delete = 20\n");

        let defaults = config.defaults();

        assert_eq!(defaults.debounce, Duration::from_secs(10));
        assert_eq!(defaults.max_delete, 20);
        assert_eq!(defaults.interval, Behavior::default().interval);
    }

    #[test]
    fn a_pair_debounce_below_the_minimum_is_a_pair_error() {
        assert_eq!(
            single_problem("debounce = \"2s\"\n"),
            ["debounce must be at least 5s"]
        );
    }

    #[test]
    fn a_pair_max_wait_below_its_debounce_is_a_pair_error() {
        assert_eq!(
            single_problem("debounce = \"1m\"\nmax_wait = \"30s\"\n"),
            ["max_wait must be at least debounce"]
        );
    }

    #[test]
    fn a_pair_interval_below_one_minute_is_a_pair_error() {
        assert_eq!(
            single_problem("interval = \"30s\"\n"),
            ["interval must be at least 1m"]
        );
    }

    #[test]
    fn a_pair_versions_keep_days_outside_the_range_is_a_pair_error() {
        for days in ["0", "3651"] {
            assert_eq!(
                single_problem(&format!("versions_keep_days = {days}\n")),
                ["versions_keep_days must be between 1 and 3650"]
            );
        }
    }

    #[test]
    fn a_pair_max_delete_outside_the_range_is_a_pair_error() {
        for percent in ["0", "101"] {
            assert_eq!(
                single_problem(&format!("max_delete = {percent}\n")),
                ["max_delete must be between 1 and 100"]
            );
        }
    }

    #[test]
    fn a_pair_value_that_is_not_in_the_list_names_the_values_that_work() {
        assert_eq!(
            single_problem("conflict_loser = \"x\"\n"),
            ["unknown value \"x\" for conflict_loser: use num, pathname or delete"]
        );
    }

    #[test]
    fn a_pair_value_of_the_wrong_type_names_the_type() {
        assert_eq!(single_problem("watch = \"yes\"\n"), ["watch must be bool"]);
        assert_eq!(
            single_problem("debounce = 30\n"),
            ["debounce must be a duration"]
        );
        assert_eq!(
            single_problem("max_delete = \"many\"\n"),
            ["max_delete must be a whole number"]
        );
    }

    #[test]
    fn a_pair_that_deletes_the_conflict_loser_without_a_winner_is_a_pair_error() {
        assert_eq!(
            single_problem("conflict_resolve = \"none\"\nconflict_loser = \"delete\"\n"),
            ["conflict_loser \"delete\" needs a conflict_resolve that picks a winner"]
        );
    }

    #[test]
    fn a_defaults_value_that_breaks_a_pairs_relation_invalidates_only_that_pair() {
        let config = parse(&format!(
            "version = 2\n[defaults]\ndebounce = \"10m\"\n\n{}\n{}max_wait = \"20m\"\n",
            pair_block("inherits", "Inherits", "/data/a", "gdrive:a"),
            pair_block("sets-its-own", "Own", "/data/b", "gdrive:b"),
        ));

        let validation = validate(&config);

        let errors: Vec<_> = validation
            .errors
            .iter()
            .map(|error| (error.pair_id.as_str(), error.message.as_str()))
            .collect();
        assert_eq!(errors, [("inherits", "max_wait must be at least debounce")]);
        assert_eq!(validation.pairs.len(), 1);
        assert_eq!(validation.pairs[0].id, "sets-its-own");
    }

    #[test]
    fn effective_names_where_each_value_came_from() {
        let config = parse(&format!(
            "version = 2\n[defaults]\ndebounce = \"10s\"\nmax_delete = 20\n\n{}max_delete = 30\n",
            pair_block("a", "A", "/a", "gdrive:a"),
        ));

        let effective = config.effective(&config.pairs()[0]);

        assert_eq!(effective.behavior.debounce, Duration::from_secs(10));
        assert_eq!(effective.behavior.max_delete, 30);
        assert_eq!(effective.origin("max_delete"), Origin::Sync);
        assert_eq!(effective.origin("debounce"), Origin::Defaults);
        assert_eq!(effective.origin("interval"), Origin::Builtin);
    }

    #[test]
    fn set_setting_creates_defaults_right_after_version() {
        let mut config = parse(&format!(
            "# top\nversion = 2\n\n{}",
            pair_block("a", "A", "/a", "gdrive:a")
        ));

        config
            .set_setting(&SettingScope::Defaults, "debounce", "10s")
            .unwrap();

        assert_eq!(
            config.to_toml(),
            format!(
                "# top\nversion = 2\n\n[defaults]\ndebounce = \"10s\"\n\n{}",
                pair_block("a", "A", "/a", "gdrive:a")
            )
        );
        assert_eq!(config.defaults().debounce, Duration::from_secs(10));
    }

    #[test]
    fn set_setting_on_a_sync_writes_the_key_into_its_table() {
        let mut config = config(&[
            pair_block("a", "A", "/a", "gdrive:a"),
            pair_block("b", "B", "/b", "gdrive:b"),
        ]);

        config
            .set_setting(&SettingScope::Sync("b".into()), "max_delete", "20")
            .unwrap();

        let reloaded = parse(&config.to_toml());
        assert_eq!(
            reloaded.effective(&reloaded.pairs()[0]).behavior.max_delete,
            50
        );
        assert_eq!(
            reloaded.effective(&reloaded.pairs()[1]).behavior.max_delete,
            20
        );
    }

    #[test]
    fn a_value_equal_to_the_inherited_one_still_creates_the_override() {
        let mut config = config(&[pair_block("a", "A", "/a", "gdrive:a")]);

        config
            .set_setting(&SettingScope::Sync("a".into()), "max_delete", "50")
            .unwrap();

        let effective = config.effective(&config.pairs()[0]);
        assert_eq!(effective.origin("max_delete"), Origin::Sync);
    }

    #[test]
    fn reset_setting_removes_the_key_and_keeps_other_comments() {
        let mut config = parse(&format!(
            "version = 2\n\n[defaults]\ndebounce = \"10s\"\n# keep me\nversions = false\n\n{}",
            pair_block("a", "A", "/a", "gdrive:a")
        ));

        config
            .reset_setting(&SettingScope::Defaults, "debounce")
            .unwrap();

        assert_eq!(
            config.to_toml(),
            format!(
                "version = 2\n\n[defaults]\n# keep me\nversions = false\n\n{}",
                pair_block("a", "A", "/a", "gdrive:a")
            )
        );
    }

    #[test]
    fn reset_setting_on_a_sync_falls_back_to_the_defaults() {
        let mut config = parse(&format!(
            "version = 2\n[defaults]\nmax_delete = 20\n\n{}max_delete = 30\n",
            pair_block("a", "A", "/a", "gdrive:a")
        ));

        config
            .reset_setting(&SettingScope::Sync("a".into()), "max_delete")
            .unwrap();

        let effective = config.effective(&config.pairs()[0]);
        assert_eq!(effective.behavior.max_delete, 20);
        assert_eq!(effective.origin("max_delete"), Origin::Defaults);
    }

    #[test]
    fn set_setting_refuses_what_the_file_would_refuse_and_changes_nothing() {
        let mut config = config(&[pair_block("a", "A", "/a", "gdrive:a")]);
        let before = config.to_toml();
        let defaults = SettingScope::Defaults;

        let errors = [
            config.set_setting(&defaults, "debounce", "2s"),
            config.set_setting(&defaults, "nope", "1"),
            config.set_setting(&SettingScope::Sync("zzz".into()), "debounce", "10s"),
            config.set_setting(&defaults, "max_wait", "1s"),
        ];

        assert_eq!(
            errors.map(|error| error.unwrap_err().to_string()),
            [
                "debounce must be at least 5s",
                "unknown setting \"nope\"",
                "no sync with id \"zzz\"",
                "max_wait must be at least debounce",
            ]
        );
        assert_eq!(config.to_toml(), before);
    }

    #[test]
    fn app_settings_are_written_to_their_own_tables_before_the_syncs() {
        let mut config = parse(&format!(
            "version = 2\n\n[defaults]\ndebounce = \"10s\"\n\n{}",
            pair_block("a", "A", "/a", "gdrive:a")
        ));
        let defaults = SettingScope::Defaults;

        config
            .set_setting(&defaults, "notifications.success", "true")
            .unwrap();
        config.set_setting(&defaults, "app.theme", "dark").unwrap();

        let written = config.to_toml();
        let reloaded = parse(&written);
        assert!(reloaded.defaults().notifications.success);
        assert_eq!(reloaded.defaults().app.theme, "dark");
        let position = |needle: &str| written.find(needle).unwrap();
        assert!(position("[defaults]") < position("[notifications]"));
        assert!(position("[notifications]") < position("[[pair]]"));
        assert!(position("[app]") < position("[[pair]]"));
    }

    #[test]
    fn resetting_the_last_app_setting_of_a_table_removes_the_table() {
        let mut config = parse(&format!(
            "version = 2\n\n[app]\ntheme = \"dark\"\n\n{}",
            pair_block("a", "A", "/a", "gdrive:a")
        ));

        config
            .reset_setting(&SettingScope::Defaults, "app.theme")
            .unwrap();

        assert!(!config.to_toml().contains("[app]"));
        assert_eq!(config.defaults().app.theme, "system");
    }

    #[test]
    fn app_settings_are_reported_with_the_defaults_overrides() {
        let config = parse(&format!(
            "version = 2\n\n[notifications]\nfailures_after = 0\n\n{}",
            pair_block("a", "A", "/a", "gdrive:a")
        ));

        assert!(
            config
                .defaults_overrides()
                .get("notifications.failures_after")
                .is_some()
        );
        assert!(config.defaults_overrides().get("app.theme").is_none());
    }

    #[test]
    fn an_app_setting_cannot_be_set_on_one_sync() {
        let mut config = config(&[pair_block("a", "A", "/a", "gdrive:a")]);
        let before = config.to_toml();

        let error = config
            .set_setting(&SettingScope::Sync("a".into()), "app.theme", "dark")
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "app.theme applies to the whole app, not to one sync"
        );
        assert_eq!(config.to_toml(), before);
    }

    #[test]
    fn an_unknown_key_in_the_notifications_or_app_table_is_refused() {
        for (table, key) in [("notifications", "sound"), ("app", "language")] {
            let text = format!("version = 2\n\n[{table}]\n{key} = true\n");

            let error = Config::parse(&text, Path::new("/home/u")).unwrap_err();

            assert_eq!(
                error.to_string(),
                format!("unknown key \"{table}.{key}\": not supported by this syncfast version")
            );
        }
    }

    #[test]
    fn a_bad_app_value_names_its_table_and_key() {
        let text = "version = 2\n\n[notifications]\nfailures_after = 101\n";

        let error = Config::parse(text, Path::new("/home/u")).unwrap_err();

        assert_eq!(
            error.to_string(),
            "config.toml: [notifications]: failures_after must be between 0 and 100"
        );
    }
}
