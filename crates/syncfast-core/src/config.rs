//! `$XDG_CONFIG_HOME/syncfast/config.toml`: parsing, validation and the
//! comment-preserving edits the GUI performs.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::filters;
use crate::paths::Paths;

const SUPPORTED_VERSION: i64 = 1;
const TOP_LEVEL_KEYS: [&str; 2] = ["version", "pair"];
const PAIR_KEYS: [&str; 7] = [
    "id", "name", "local", "remote", "enabled", "presets", "filters",
];

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

/// Why [`Config::edit`] did not write.
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
        if let Some(version) = document.get("version").and_then(Item::as_integer)
            && version != SUPPORTED_VERSION
        {
            return Err(GlobalConfigError::UnsupportedVersion(version));
        }
        normalize_pair_array(&mut document);
        reject_unknown_keys(&document)?;
        let mut raw: Raw =
            toml::from_str(text).map_err(|err| GlobalConfigError::Toml(err.to_string()))?;
        for pair in &mut raw.pair {
            pair.local = expand_tilde(&pair.local, home);
        }
        Ok(Self {
            document,
            pairs: raw.pair,
        })
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
        let mut messages: Vec<Option<String>> = self
            .pairs
            .iter()
            .map(|pair| {
                let duplicate = id_counts.get(pair.id.as_str()).copied().unwrap_or(0) > 1;
                problem_before_overlap(pair, duplicate, &data_dir)
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

    /// Loads the config, applies `change` and saves, all under an exclusive
    /// lock on `config.lock`, so concurrent edits (the GUI, the CLI, an
    /// agent) never overwrite each other. Nothing is written when `change`
    /// leaves the document as it was, so it must validate before it mutates.
    ///
    /// # Errors
    /// The lock cannot be taken, the file cannot be loaded, or the changed
    /// config cannot be saved.
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
        let file = paths.config_file();
        let target = match tokio::fs::canonicalize(&file).await {
            Ok(target) => target,
            Err(_) => {
                if let Ok(meta) = tokio::fs::symlink_metadata(&file).await
                    && meta.is_symlink()
                {
                    let target = tokio::fs::read_link(&file).await.unwrap_or(file);
                    return Err(ConfigSaveError::DanglingLink { target });
                }
                file
            }
        };
        let contents = self.to_toml();
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
/// released when the returned file is dropped.
async fn lock_config(paths: &Paths) -> Result<std::fs::File, ConfigSaveError> {
    let dir = paths.config_dir();
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
fn replace_value(table: &mut Table, key: &str, mut new: Value) {
    match table.get_mut(key) {
        Some(item) => {
            if let Some(old) = item.as_value() {
                *new.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(new);
        }
        None => {
            table[key] = Item::Value(new);
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

fn reject_unknown_keys(document: &DocumentMut) -> Result<(), GlobalConfigError> {
    let unknown = |table: &Table, allowed: &[&str]| {
        table
            .iter()
            .map(|(key, _)| key)
            .find(|key| !allowed.contains(key))
            .map(|key| GlobalConfigError::UnknownKey(key.to_owned()))
    };
    if let Some(err) = unknown(document.as_table(), &TOP_LEVEL_KEYS) {
        return Err(err);
    }
    if let Some(tables) = document.get("pair").and_then(Item::as_array_of_tables) {
        for table in tables {
            if let Some(err) = unknown(table, &PAIR_KEYS) {
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
fn problem_before_overlap(pair: &Pair, duplicate_id: bool, data_dir: &Path) -> Option<String> {
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
    None
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
        let text = format!("version = 1\n\n{}", blocks.join("\n"));
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
    fn version_other_than_one_is_rejected_even_with_unknown_keys() {
        let err = Config::parse("version = 2\nfuture = true\n", Path::new(HOME)).unwrap_err();

        assert_eq!(err.to_string(), "unsupported config version 2");
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
        assert_eq!(config.to_toml(), "version = 1\n");
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
            })
            .unwrap();

        config.save(&paths).await.unwrap();

        assert!(
            std::fs::symlink_metadata(paths.config_file())
                .unwrap()
                .is_symlink()
        );
        let stowed = std::fs::read_to_string(&target).unwrap();
        assert!(stowed.starts_with("# stowed\nversion = 1\n"));
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
}
