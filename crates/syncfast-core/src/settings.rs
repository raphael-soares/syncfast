//! The registry of every behavior key: its name, type, limits and how it
//! reads and writes a [`Behavior`]. `config.toml`, the CLI, MCP and the GUI
//! all validate through it, so a value means the same thing everywhere.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::Serialize;
use toml_edit::{Item, TableLike};

use crate::behavior::{
    Bandwidth, Behavior, ConflictLoser, ConflictResolve, ResyncMode, ReviewLevel, SyncMode,
    format_duration, parse_duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Duration(Duration),
    Int(i64),
    Choice(&'static str),
    Bandwidth(Bandwidth),
    Text(String),
}

impl fmt::Display for Value {
    /// The canonical text: what `config.toml` stores and the CLI prints.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Duration(value) => f.write_str(&format_duration(*value)),
            Self::Int(value) => write!(f, "{value}"),
            Self::Choice(value) => f.write_str(value),
            Self::Bandwidth(value) => f.write_str(&value.format()),
            Self::Text(value) => f.write_str(value),
        }
    }
}

impl Value {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Bool(value) => serde_json::Value::Bool(*value),
            Self::Int(value) => serde_json::Value::from(*value),
            Self::Duration(_) | Self::Choice(_) | Self::Bandwidth(_) | Self::Text(_) => {
                serde_json::Value::String(self.to_string())
            }
        }
    }

    pub(crate) fn to_toml(&self) -> toml_edit::Value {
        match self {
            Self::Bool(value) => toml_edit::Value::from(*value),
            Self::Int(value) => toml_edit::Value::from(*value),
            Self::Duration(_) | Self::Choice(_) | Self::Bandwidth(_) | Self::Text(_) => {
                toml_edit::Value::from(self.to_string())
            }
        }
    }
}

/// How a key reads and writes a [`Behavior`], by type.
#[derive(Debug)]
pub enum Access {
    Bool {
        get: fn(&Behavior) -> bool,
        set: fn(&mut Behavior, bool),
    },
    Duration {
        min: Option<Duration>,
        get: fn(&Behavior) -> Duration,
        set: fn(&mut Behavior, Duration),
    },
    Int {
        min: i64,
        max: i64,
        get: fn(&Behavior) -> i64,
        set: fn(&mut Behavior, i64),
    },
    Choice {
        names: &'static [&'static str],
        get: fn(&Behavior) -> &'static str,
        set: fn(&mut Behavior, &str),
    },
    Bandwidth {
        get: fn(&Behavior) -> Bandwidth,
        set: fn(&mut Behavior, Bandwidth),
    },
    Text {
        get: fn(&Behavior) -> String,
        set: fn(&mut Behavior, &str),
    },
}

#[derive(Debug)]
pub struct KeyDef {
    pub name: &'static str,
    /// The key is only valid in a `[[pair]]`, never in `[defaults]`.
    pub per_sync_only: bool,
    /// Whether the key does anything in a sync of this mode. A key that does
    /// not apply is still accepted and kept, so changing mode loses nothing.
    pub applies_to: fn(SyncMode) -> bool,
    pub access: Access,
    /// Whether going from the first behavior to the second makes the sync
    /// less safe: only the user may do it.
    pub lowers_safety: Option<fn(&Behavior, &Behavior) -> bool>,
}

macro_rules! choice {
    ($field:ident, $ty:ident) => {
        Access::Choice {
            names: $ty::NAMES,
            get: |behavior| behavior.$field.name(),
            set: |behavior, name| {
                if let Some(value) = $ty::from_name(name) {
                    behavior.$field = value;
                }
            },
        }
    };
}

fn always(_: SyncMode) -> bool {
    true
}

fn watches(mode: SyncMode) -> bool {
    mode.local_is_source()
}

fn bisync_only(mode: SyncMode) -> bool {
    mode == SyncMode::Bisync
}

fn keeps_versions(mode: SyncMode) -> bool {
    mode != SyncMode::Upload
}

fn deletes(mode: SyncMode) -> bool {
    mode != SyncMode::Upload && mode != SyncMode::Download
}

fn uploads(mode: SyncMode) -> bool {
    mode.uploads()
}

fn downloads(mode: SyncMode) -> bool {
    mode.downloads()
}

/// In the order the docs and `settings` list them.
pub static KEYS: [KeyDef; 15] = [
    KeyDef {
        name: "watch",
        per_sync_only: false,
        applies_to: watches,
        access: Access::Bool {
            get: |behavior| behavior.watch,
            set: |behavior, value| behavior.watch = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "debounce",
        per_sync_only: false,
        applies_to: watches,
        access: Access::Duration {
            min: Some(Duration::from_secs(5)),
            get: |behavior| behavior.debounce,
            set: |behavior, value| behavior.debounce = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "max_wait",
        per_sync_only: false,
        applies_to: watches,
        access: Access::Duration {
            min: None,
            get: |behavior| behavior.max_wait,
            set: |behavior, value| behavior.max_wait = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "interval",
        per_sync_only: false,
        applies_to: always,
        access: Access::Duration {
            min: Some(Duration::from_mins(1)),
            get: |behavior| behavior.interval,
            set: |behavior, value| behavior.interval = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "limit_upload",
        per_sync_only: false,
        applies_to: uploads,
        access: Access::Bandwidth {
            get: |behavior| behavior.limit_upload,
            set: |behavior, value| behavior.limit_upload = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "limit_download",
        per_sync_only: false,
        applies_to: downloads,
        access: Access::Bandwidth {
            get: |behavior| behavior.limit_download,
            set: |behavior, value| behavior.limit_download = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "conflict_resolve",
        per_sync_only: false,
        applies_to: bisync_only,
        access: choice!(conflict_resolve, ConflictResolve),
        lowers_safety: None,
    },
    KeyDef {
        name: "conflict_loser",
        per_sync_only: false,
        applies_to: bisync_only,
        access: choice!(conflict_loser, ConflictLoser),
        lowers_safety: Some(|before, after| {
            before.conflict_loser != ConflictLoser::Delete
                && after.conflict_loser == ConflictLoser::Delete
        }),
    },
    KeyDef {
        name: "versions",
        per_sync_only: false,
        applies_to: keeps_versions,
        access: Access::Bool {
            get: |behavior| behavior.versions,
            set: |behavior, value| behavior.versions = value,
        },
        lowers_safety: Some(|before, after| before.versions && !after.versions),
    },
    KeyDef {
        name: "versions_keep_days",
        per_sync_only: false,
        applies_to: keeps_versions,
        access: Access::Int {
            min: 1,
            max: 3650,
            get: |behavior| i64::from(behavior.versions_keep_days),
            set: |behavior, value| {
                behavior.versions_keep_days = u32::try_from(value).unwrap_or(u32::MAX);
            },
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "max_delete",
        per_sync_only: false,
        applies_to: deletes,
        access: Access::Int {
            min: 1,
            max: 100,
            get: |behavior| i64::from(behavior.max_delete),
            set: |behavior, value| behavior.max_delete = u8::try_from(value).unwrap_or(u8::MAX),
        },
        lowers_safety: Some(|before, after| after.max_delete > before.max_delete),
    },
    KeyDef {
        name: "resync_mode",
        per_sync_only: false,
        applies_to: bisync_only,
        access: choice!(resync_mode, ResyncMode),
        lowers_safety: None,
    },
    KeyDef {
        name: "empty_dirs",
        per_sync_only: false,
        applies_to: always,
        access: Access::Bool {
            get: |behavior| behavior.empty_dirs,
            set: |behavior, value| behavior.empty_dirs = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "review",
        per_sync_only: false,
        applies_to: always,
        access: choice!(review, ReviewLevel),
        lowers_safety: Some(|before, after| after.review.is_less_safe_than(before.review)),
    },
    KeyDef {
        name: "mode",
        per_sync_only: true,
        applies_to: always,
        access: choice!(mode, SyncMode),
        lowers_safety: Some(|before, after| after.mode.is_mirror() && after.mode != before.mode),
    },
];

macro_rules! notification_flag {
    ($name:literal, $field:ident) => {
        KeyDef {
            name: $name,
            per_sync_only: false,
            applies_to: always,
            access: Access::Bool {
                get: |behavior| behavior.notifications.$field,
                set: |behavior, value| behavior.notifications.$field = value,
            },
            lowers_safety: None,
        }
    };
}

/// The keys that belong to the whole app, in `[notifications]` and `[app]`.
/// They live on [`Behavior`] too, so one round carries them, but they have
/// no per-sync value and never appear in `[defaults]`.
pub static APP_KEYS: [KeyDef; 6] = [
    notification_flag!("notifications.needs_action", needs_action),
    notification_flag!("notifications.conflicts", conflicts),
    KeyDef {
        name: "notifications.failures_after",
        per_sync_only: false,
        applies_to: always,
        access: Access::Int {
            min: 0,
            max: 100,
            get: |behavior| i64::from(behavior.notifications.failures_after),
            set: |behavior, value| {
                behavior.notifications.failures_after = u8::try_from(value).unwrap_or(u8::MAX);
            },
        },
        lowers_safety: None,
    },
    notification_flag!("notifications.success", success),
    KeyDef {
        name: "app.autostart",
        per_sync_only: false,
        applies_to: always,
        access: Access::Bool {
            get: |behavior| behavior.app.autostart,
            set: |behavior, value| behavior.app.autostart = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "app.theme",
        per_sync_only: false,
        applies_to: always,
        access: Access::Text {
            get: |behavior| behavior.app.theme.clone(),
            set: |behavior, value| value.clone_into(&mut behavior.app.theme),
        },
        lowers_safety: None,
    },
];

#[must_use]
pub fn key(name: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|def| def.name == name)
}

#[must_use]
pub fn app_key(name: &str) -> Option<&'static KeyDef> {
    APP_KEYS.iter().find(|def| def.name == name)
}

#[must_use]
pub fn per_sync_only_message(key: &str) -> String {
    format!("{key} is set per sync, not in [defaults]")
}

/// Problems that only exist between keys, judged on a resolved behavior.
#[must_use]
pub fn relation_problem(behavior: &Behavior) -> Option<String> {
    if watches(behavior.mode) && behavior.max_wait < behavior.debounce {
        return Some("max_wait must be at least debounce".to_owned());
    }
    if bisync_only(behavior.mode)
        && behavior.conflict_resolve == ConflictResolve::None
        && behavior.conflict_loser == ConflictLoser::Delete
    {
        return Some(
            "conflict_loser \"delete\" needs a conflict_resolve that picks a winner".to_owned(),
        );
    }
    None
}

/// The keys of `behavior` that are less safe than the built-in value.
#[must_use]
pub fn lowered_safety(behavior: &Behavior) -> Vec<&'static str> {
    let builtin = Behavior::default();
    KEYS.iter()
        .filter(|def| (def.applies_to)(behavior.mode) && def.lowers(&builtin, behavior))
        .map(|def| def.name)
        .collect()
}

impl KeyDef {
    #[must_use]
    pub fn get(&self, behavior: &Behavior) -> Value {
        match &self.access {
            Access::Bool { get, .. } => Value::Bool(get(behavior)),
            Access::Duration { get, .. } => Value::Duration(get(behavior)),
            Access::Int { get, .. } => Value::Int(get(behavior)),
            Access::Choice { get, .. } => Value::Choice(get(behavior)),
            Access::Bandwidth { get, .. } => Value::Bandwidth(get(behavior)),
            Access::Text { get, .. } => Value::Text(get(behavior)),
        }
    }

    pub fn apply(&self, behavior: &mut Behavior, value: &Value) {
        match (&self.access, value) {
            (Access::Bool { set, .. }, Value::Bool(value)) => set(behavior, *value),
            (Access::Duration { set, .. }, Value::Duration(value)) => set(behavior, *value),
            (Access::Int { set, .. }, Value::Int(value)) => set(behavior, *value),
            (Access::Choice { set, .. }, Value::Choice(value)) => set(behavior, value),
            (Access::Bandwidth { set, .. }, Value::Bandwidth(value)) => set(behavior, *value),
            (Access::Text { set, .. }, Value::Text(value)) => set(behavior, value),
            _ => {}
        }
    }

    #[must_use]
    pub fn builtin(&self) -> Value {
        self.get(&Behavior::default())
    }

    #[must_use]
    pub fn lowers(&self, before: &Behavior, after: &Behavior) -> bool {
        self.lowers_safety
            .is_some_and(|lowers| lowers(before, after))
    }

    /// The value written as text, as the CLI, MCP and the GUI take it.
    ///
    /// # Errors
    /// The message that says what is wrong with the text.
    pub fn parse_text(&self, text: &str) -> Result<Value, String> {
        match &self.access {
            Access::Bool { .. } => match text {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(self.type_error()),
            },
            Access::Duration { .. } => {
                let duration = parse_duration(text).map_err(|err| err.to_string())?;
                self.check_duration(duration)
            }
            Access::Int { .. } => {
                let number = text.parse::<i64>().map_err(|_| self.type_error())?;
                self.check_int(number)
            }
            Access::Choice { .. } => self.check_choice(text),
            Access::Bandwidth { .. } => self.check_bandwidth(text),
            Access::Text { .. } => self.check_text(text),
        }
    }

    /// The value as `config.toml` holds it: the TOML type has to fit.
    ///
    /// # Errors
    /// The message that says what is wrong with the value.
    pub fn parse_item(&self, item: &Item) -> Result<Value, String> {
        match &self.access {
            Access::Bool { .. } => item
                .as_bool()
                .map(Value::Bool)
                .ok_or_else(|| self.type_error()),
            Access::Duration { .. } => {
                let text = item.as_str().ok_or_else(|| self.type_error())?;
                self.parse_text(text)
            }
            Access::Int { .. } => {
                let number = item.as_integer().ok_or_else(|| self.type_error())?;
                self.check_int(number)
            }
            Access::Choice { .. } => match item.as_str() {
                Some(text) => self.check_choice(text),
                None => self.check_choice(item.to_string().trim()),
            },
            Access::Bandwidth { .. } => {
                let text = item.as_str().ok_or_else(|| self.type_error())?;
                self.check_bandwidth(text)
            }
            Access::Text { .. } => {
                let text = item.as_str().ok_or_else(|| self.type_error())?;
                self.check_text(text)
            }
        }
    }

    /// The name the user sees in a message: without the table of an app key.
    #[must_use]
    pub fn short(&self) -> &'static str {
        self.name.rsplit('.').next().unwrap_or(self.name)
    }

    fn type_error(&self) -> String {
        let noun = match self.access {
            Access::Bool { .. } => "bool",
            Access::Duration { .. } => "a duration",
            Access::Int { .. } | Access::Choice { .. } => "a whole number",
            Access::Text { .. } => "text",
            Access::Bandwidth { .. } => {
                return format!(
                    "{} must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M",
                    self.short()
                );
            }
        };
        format!("{} must be {noun}", self.short())
    }

    fn check_text(&self, text: &str) -> Result<Value, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(format!("{} must not be empty", self.short()));
        }
        Ok(Value::Text(text.to_owned()))
    }

    fn check_duration(&self, duration: Duration) -> Result<Value, String> {
        if let Access::Duration { min: Some(min), .. } = self.access
            && duration < min
        {
            return Err(format!(
                "{} must be at least {}",
                self.short(),
                format_duration(min)
            ));
        }
        Ok(Value::Duration(duration))
    }

    fn check_int(&self, number: i64) -> Result<Value, String> {
        if let Access::Int { min, max, .. } = self.access
            && !(min..=max).contains(&number)
        {
            return Err(format!("{} must be between {min} and {max}", self.short()));
        }
        Ok(Value::Int(number))
    }

    fn check_bandwidth(&self, text: &str) -> Result<Value, String> {
        Bandwidth::parse(text)
            .map(Value::Bandwidth)
            .ok_or_else(|| self.type_error())
    }

    fn check_choice(&self, text: &str) -> Result<Value, String> {
        let Access::Choice { names, .. } = self.access else {
            return Err(self.type_error());
        };
        names
            .iter()
            .find(|name| **name == text)
            .copied()
            .map(Value::Choice)
            .ok_or_else(|| {
                format!(
                    "unknown value \"{text}\" for {}: use {}",
                    self.short(),
                    list_with_or(names)
                )
            })
    }
}

fn list_with_or(names: &[&str]) -> String {
    match names.split_last() {
        Some((last, [])) => (*last).to_owned(),
        Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
        None => String::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Builtin,
    Defaults,
    Sync,
}

impl Origin {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Defaults => "defaults",
            Self::Sync => "sync",
        }
    }
}

/// The behavior of a sync and where each value came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effective {
    pub behavior: Behavior,
    pub origins: BTreeMap<&'static str, Origin>,
}

impl Effective {
    #[must_use]
    pub fn origin(&self, key: &str) -> Origin {
        self.origins.get(key).copied().unwrap_or(Origin::Builtin)
    }
}

/// The valid values one table of `config.toml` holds, plus the first problem
/// found in the others. A table with a problem still carries its valid
/// values, but the config reports it as an error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    values: BTreeMap<&'static str, Value>,
    problem: Option<String>,
}

impl Overrides {
    pub(crate) fn read(table: &dyn TableLike, in_defaults: bool) -> Self {
        let mut overrides = Self::default();
        for def in &KEYS {
            let Some(item) = table.get(def.name) else {
                continue;
            };
            match read_key(def, item, in_defaults) {
                Ok(value) => {
                    overrides.values.insert(def.name, value);
                }
                Err(message) => {
                    overrides.problem.get_or_insert(message);
                }
            }
        }
        overrides
    }

    /// Reads the app keys from the document root, where `[notifications]` and
    /// `[app]` live. A problem names its table.
    pub(crate) fn read_app(root: &dyn TableLike) -> Self {
        let mut overrides = Self::default();
        for def in &APP_KEYS {
            let Some((table, short)) = def.name.split_once('.') else {
                continue;
            };
            let Some(item) = root
                .get(table)
                .and_then(Item::as_table_like)
                .and_then(|table| table.get(short))
            else {
                continue;
            };
            match def.parse_item(item) {
                Ok(value) => {
                    overrides.values.insert(def.name, value);
                }
                Err(message) => {
                    overrides
                        .problem
                        .get_or_insert(format!("[{table}]: {message}"));
                }
            }
        }
        overrides
    }

    /// This table's values on top of `other`'s.
    pub(crate) fn merged_over(&self, other: &Self) -> Self {
        let mut values = other.values.clone();
        values.extend(self.values.iter().map(|(key, value)| (*key, value.clone())));
        Self {
            values,
            problem: other.problem.clone().or_else(|| self.problem.clone()),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    #[must_use]
    pub fn contains(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    #[must_use]
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    pub fn apply_to(&self, behavior: &mut Behavior) {
        for def in KEYS.iter().chain(&APP_KEYS) {
            if let Some(value) = self.values.get(def.name) {
                def.apply(behavior, value);
            }
        }
    }
}

fn read_key(def: &KeyDef, item: &Item, in_defaults: bool) -> Result<Value, String> {
    if in_defaults && def.per_sync_only {
        return Err(per_sync_only_message(def.name));
    }
    def.parse_item(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Policy;

    fn def(name: &str) -> &'static KeyDef {
        key(name).unwrap()
    }

    fn text(name: &str, text: &str) -> Result<String, String> {
        def(name).parse_text(text).map(|value| value.to_string())
    }

    #[test]
    fn every_key_reads_back_the_builtin_default() {
        let behavior = Behavior::default();

        let values: Vec<String> = KEYS
            .iter()
            .map(|def| format!("{}={}", def.name, def.get(&behavior)))
            .collect();

        assert_eq!(
            values,
            [
                "watch=true",
                "debounce=30s",
                "max_wait=5m",
                "interval=5m",
                "limit_upload=off",
                "limit_download=off",
                "conflict_resolve=newer",
                "conflict_loser=num",
                "versions=true",
                "versions_keep_days=30",
                "max_delete=50",
                "resync_mode=newer",
                "empty_dirs=true",
                "review=deletions",
                "mode=bisync",
            ]
        );
    }

    #[test]
    fn every_key_applies_a_value_to_its_own_field() {
        let mut behavior = Behavior::default();
        for (name, value) in [
            ("watch", "false"),
            ("debounce", "10s"),
            ("max_wait", "10m"),
            ("interval", "1h"),
            ("limit_upload", "2m"),
            ("limit_download", "512K"),
            ("conflict_resolve", "local"),
            ("conflict_loser", "pathname"),
            ("versions", "false"),
            ("versions_keep_days", "7"),
            ("max_delete", "10"),
            ("resync_mode", "remote"),
            ("empty_dirs", "false"),
            ("review", "automatic"),
            ("mode", "mirror_up"),
        ] {
            let def = def(name);
            def.apply(&mut behavior, &def.parse_text(value).unwrap());
        }

        assert_eq!(
            behavior,
            Behavior {
                watch: false,
                debounce: Duration::from_secs(10),
                max_wait: Duration::from_mins(10),
                interval: Duration::from_hours(1),
                limit_upload: Bandwidth::KiBPerSec(2048),
                limit_download: Bandwidth::KiBPerSec(512),
                conflict_resolve: ConflictResolve::Local,
                conflict_loser: ConflictLoser::Pathname,
                versions: false,
                versions_keep_days: 7,
                max_delete: 10,
                resync_mode: ResyncMode::Remote,
                mode: SyncMode::MirrorUp,
                empty_dirs: false,
                review: ReviewLevel::Automatic,
                ..Behavior::default()
            }
        );
    }

    #[test]
    fn values_are_written_in_their_canonical_form() {
        assert_eq!(text("debounce", "300s").unwrap(), "5m");
        assert_eq!(text("interval", "60m").unwrap(), "1h");
        assert_eq!(text("max_wait", "90s").unwrap(), "90s");
        assert_eq!(text("max_delete", "25").unwrap(), "25");
        assert_eq!(text("watch", "false").unwrap(), "false");
        assert_eq!(text("watch", "true").unwrap(), "true");
        assert_eq!(text("watch", "yes").unwrap_err(), "watch must be bool");
    }

    #[test]
    fn debounce_below_five_seconds_is_refused() {
        assert_eq!(
            text("debounce", "4s").unwrap_err(),
            "debounce must be at least 5s"
        );
        assert_eq!(text("debounce", "5s").unwrap(), "5s");
    }

    #[test]
    fn interval_below_one_minute_is_refused() {
        assert_eq!(
            text("interval", "59s").unwrap_err(),
            "interval must be at least 1m"
        );
        assert_eq!(text("interval", "1m").unwrap(), "1m");
    }

    const SPEED_MESSAGE: &str =
        "must be off or a speed: a whole number followed by K, M or G, for example 500K or 2M";

    #[test]
    fn a_speed_is_written_in_its_canonical_form() {
        assert_eq!(text("limit_upload", "1024K").unwrap(), "1M");
        assert_eq!(text("limit_upload", "2m").unwrap(), "2M");
        assert_eq!(text("limit_download", "1024M").unwrap(), "1G");
        assert_eq!(text("limit_download", "1536K").unwrap(), "1536K");
        assert_eq!(text("limit_upload", "off").unwrap(), "off");
    }

    #[test]
    fn a_bad_speed_gets_the_one_message_of_its_key() {
        for bad in ["0", "0K", "1.5M", "10", "-1K", "5T", ""] {
            assert_eq!(
                text("limit_upload", bad).unwrap_err(),
                format!("limit_upload {SPEED_MESSAGE}"),
                "{bad:?}"
            );
            assert_eq!(
                text("limit_download", bad).unwrap_err(),
                format!("limit_download {SPEED_MESSAGE}"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_speed_in_the_config_is_a_string_and_a_number_is_refused() {
        let upload = def("limit_upload");

        assert_eq!(
            upload.parse_item(&item("\"2M\"")).unwrap(),
            Value::Bandwidth(Bandwidth::KiBPerSec(2048))
        );
        assert_eq!(
            upload.parse_item(&item("2")).unwrap_err(),
            format!("limit_upload {SPEED_MESSAGE}")
        );
    }

    #[test]
    fn bandwidth_limits_do_not_lower_safety() {
        let limited = Behavior {
            limit_upload: Bandwidth::KiBPerSec(512),
            limit_download: Bandwidth::KiBPerSec(512),
            ..Behavior::default()
        };

        assert!(lowered_safety(&limited).is_empty());
        assert!(!def("limit_upload").lowers(&Behavior::default(), &limited));
    }

    #[test]
    fn versions_keep_days_outside_1_to_3650_is_refused() {
        for bad in ["0", "3651"] {
            assert_eq!(
                text("versions_keep_days", bad).unwrap_err(),
                "versions_keep_days must be between 1 and 3650"
            );
        }
        assert_eq!(text("versions_keep_days", "3650").unwrap(), "3650");
        assert_eq!(text("versions_keep_days", "1").unwrap(), "1");
    }

    #[test]
    fn max_delete_outside_1_to_100_is_refused() {
        for bad in ["0", "101"] {
            assert_eq!(
                text("max_delete", bad).unwrap_err(),
                "max_delete must be between 1 and 100"
            );
        }
        assert_eq!(text("max_delete", "100").unwrap(), "100");
    }

    fn app_text(name: &str, text: &str) -> Result<String, String> {
        app_key(name)
            .unwrap()
            .parse_text(text)
            .map(|value| value.to_string())
    }

    #[test]
    fn the_app_keys_read_back_their_defaults() {
        let behavior = Behavior::default();

        let values: Vec<String> = APP_KEYS
            .iter()
            .map(|def| format!("{}={}", def.name, def.get(&behavior)))
            .collect();

        assert_eq!(
            values,
            [
                "notifications.needs_action=true",
                "notifications.conflicts=true",
                "notifications.failures_after=3",
                "notifications.success=false",
                "app.autostart=false",
                "app.theme=system",
            ]
        );
    }

    #[test]
    fn the_app_keys_apply_to_their_own_field() {
        let mut behavior = Behavior::default();

        for (name, value) in [
            ("notifications.needs_action", "false"),
            ("notifications.conflicts", "false"),
            ("notifications.failures_after", "0"),
            ("notifications.success", "true"),
            ("app.autostart", "true"),
            ("app.theme", "dark"),
        ] {
            let def = app_key(name).unwrap();
            def.apply(&mut behavior, &def.parse_text(value).unwrap());
        }

        assert_eq!(
            behavior.notifications,
            Policy {
                needs_action: false,
                conflicts: false,
                failures_after: 0,
                success: true,
            }
        );
        assert!(behavior.app.autostart);
        assert_eq!(behavior.app.theme, "dark");
    }

    #[test]
    fn failures_after_outside_0_to_100_is_refused() {
        for bad in ["-1", "101"] {
            assert_eq!(
                app_text("notifications.failures_after", bad).unwrap_err(),
                "failures_after must be between 0 and 100"
            );
        }
        assert_eq!(app_text("notifications.failures_after", "0").unwrap(), "0");
    }

    #[test]
    fn an_empty_theme_is_refused() {
        assert_eq!(
            app_text("app.theme", "  ").unwrap_err(),
            "theme must not be empty"
        );
        assert_eq!(app_text("app.theme", " dark ").unwrap(), "dark");
    }

    #[test]
    fn the_app_keys_are_not_sync_settings() {
        assert!(key("app.theme").is_none());
        assert!(key("notifications.success").is_none());
    }

    #[test]
    fn an_unknown_choice_lists_the_values_that_work() {
        assert_eq!(
            text("conflict_resolve", "biggest").unwrap_err(),
            "unknown value \"biggest\" for conflict_resolve: use none, newer, older, larger, smaller, local or remote"
        );
        assert_eq!(
            text("conflict_loser", "x").unwrap_err(),
            "unknown value \"x\" for conflict_loser: use num, pathname or delete"
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_names_the_type_wanted() {
        assert_eq!(text("watch", "yes").unwrap_err(), "watch must be bool");
        assert_eq!(
            text("max_delete", "many").unwrap_err(),
            "max_delete must be a whole number"
        );
        assert_eq!(
            text("debounce", "soon").unwrap_err(),
            "invalid duration \"soon\": use a number followed by s, m or h, for example 30s, 5m or 1h30m"
        );
    }

    fn item(source: &str) -> Item {
        let document: toml_edit::DocumentMut = format!("k = {source}").parse().unwrap();
        document["k"].clone()
    }

    #[test]
    fn toml_values_must_have_the_right_type() {
        assert_eq!(
            def("watch").parse_item(&item("\"true\"")).unwrap_err(),
            "watch must be bool"
        );
        assert_eq!(
            def("debounce").parse_item(&item("30")).unwrap_err(),
            "debounce must be a duration"
        );
        assert_eq!(
            def("max_delete").parse_item(&item("\"50\"")).unwrap_err(),
            "max_delete must be a whole number"
        );
        assert_eq!(
            def("max_delete").parse_item(&item("50.5")).unwrap_err(),
            "max_delete must be a whole number"
        );
        assert_eq!(
            def("conflict_resolve").parse_item(&item("5")).unwrap_err(),
            "unknown value \"5\" for conflict_resolve: use none, newer, older, larger, smaller, local or remote"
        );
    }

    #[test]
    fn toml_values_of_the_right_type_are_read() {
        assert_eq!(
            def("watch").parse_item(&item("false")).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            def("debounce").parse_item(&item("\"10s\"")).unwrap(),
            Value::Duration(Duration::from_secs(10))
        );
        assert_eq!(
            def("max_delete").parse_item(&item("20")).unwrap(),
            Value::Int(20)
        );
        assert_eq!(
            def("resync_mode").parse_item(&item("\"older\"")).unwrap(),
            Value::Choice("older")
        );
    }

    #[test]
    fn a_per_sync_key_is_refused_in_defaults_only() {
        let per_sync = KeyDef {
            name: "mode",
            per_sync_only: true,
            applies_to: always,
            access: Access::Bool {
                get: |behavior| behavior.watch,
                set: |behavior, value| behavior.watch = value,
            },
            lowers_safety: None,
        };

        assert_eq!(
            read_key(&per_sync, &item("true"), true).unwrap_err(),
            "mode is set per sync, not in [defaults]"
        );
        assert_eq!(
            read_key(&per_sync, &item("true"), false).unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn a_table_keeps_valid_values_and_reports_the_first_problem() {
        let document: toml_edit::DocumentMut =
            "[t]\ndebounce = \"2s\"\nwatch = false\nmax_delete = 500\n"
                .parse()
                .unwrap();

        let overrides = Overrides::read(document["t"].as_table().unwrap(), false);

        assert_eq!(overrides.get("watch"), Some(&Value::Bool(false)));
        assert!(!overrides.contains("debounce"));
        assert_eq!(overrides.problem(), Some("debounce must be at least 5s"));
    }

    #[test]
    fn overrides_apply_only_the_keys_they_hold() {
        let document: toml_edit::DocumentMut = "[t]\nmax_delete = 20\n".parse().unwrap();
        let overrides = Overrides::read(document["t"].as_table().unwrap(), false);
        let mut behavior = Behavior::default();

        overrides.apply_to(&mut behavior);

        assert_eq!(
            behavior,
            Behavior {
                max_delete: 20,
                ..Behavior::default()
            }
        );
    }

    #[test]
    fn relation_problems_are_named() {
        let loose = Behavior {
            debounce: Duration::from_mins(10),
            ..Behavior::default()
        };
        let no_winner = Behavior {
            conflict_resolve: ConflictResolve::None,
            conflict_loser: ConflictLoser::Delete,
            ..Behavior::default()
        };

        assert_eq!(relation_problem(&Behavior::default()), None);
        assert_eq!(
            relation_problem(&loose).as_deref(),
            Some("max_wait must be at least debounce")
        );
        assert_eq!(
            relation_problem(&no_winner).as_deref(),
            Some("conflict_loser \"delete\" needs a conflict_resolve that picks a winner")
        );
    }

    #[test]
    fn max_wait_equal_to_debounce_is_allowed() {
        let equal = Behavior {
            debounce: Duration::from_mins(5),
            ..Behavior::default()
        };

        assert_eq!(relation_problem(&equal), None);
    }

    #[test]
    fn keys_a_mode_ignores_raise_no_relation_problem_and_no_safety_warning() {
        let stored = Behavior {
            debounce: Duration::from_mins(10),
            conflict_resolve: ConflictResolve::None,
            conflict_loser: ConflictLoser::Delete,
            max_delete: 80,
            versions: false,
            ..Behavior::default()
        };
        let download = Behavior {
            mode: SyncMode::Download,
            ..stored.clone()
        };
        let mirror_down = Behavior {
            mode: SyncMode::MirrorDown,
            ..stored.clone()
        };

        assert_eq!(relation_problem(&download), None);
        assert_eq!(
            relation_problem(&mirror_down).as_deref(),
            None,
            "a mirror pulls: the debounce does not matter"
        );
        let upload = Behavior {
            mode: SyncMode::Upload,
            ..stored.clone()
        };
        assert!(lowered_safety(&upload).is_empty());
        assert_eq!(lowered_safety(&download), ["versions"]);
        assert_eq!(lowered_safety(&mirror_down), ["versions", "max_delete"]);
        assert!(relation_problem(&stored).is_some());
    }

    #[test]
    fn lowered_safety_lists_keys_less_safe_than_the_builtin_default() {
        let behavior = Behavior {
            max_delete: 80,
            versions: false,
            conflict_resolve: ConflictResolve::Newer,
            conflict_loser: ConflictLoser::Delete,
            ..Behavior::default()
        };

        assert_eq!(
            lowered_safety(&behavior),
            ["conflict_loser", "versions", "max_delete"]
        );
        assert!(lowered_safety(&Behavior::default()).is_empty());
        assert!(
            lowered_safety(&Behavior {
                max_delete: 10,
                ..Behavior::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn only_the_safety_keys_can_lower_safety() {
        let names: Vec<_> = KEYS
            .iter()
            .filter(|def| def.lowers_safety.is_some())
            .map(|def| def.name)
            .collect();

        assert_eq!(
            names,
            ["conflict_loser", "versions", "max_delete", "review", "mode"]
        );
    }

    #[test]
    fn review_lowers_safety_only_when_it_asks_less_often() {
        let review = def("review");
        let at = |review| Behavior {
            review,
            ..Behavior::default()
        };
        let levels = [
            ReviewLevel::Manual,
            ReviewLevel::Deletions,
            ReviewLevel::Automatic,
        ];

        for (from, before) in levels.iter().enumerate() {
            for (to, after) in levels.iter().enumerate() {
                assert_eq!(
                    review.lowers(&at(*before), &at(*after)),
                    to > from,
                    "{before:?} to {after:?}"
                );
            }
        }
        assert_eq!(lowered_safety(&at(ReviewLevel::Automatic)), ["review"]);
        assert!(lowered_safety(&at(ReviewLevel::Manual)).is_empty());
    }

    #[test]
    fn a_change_to_the_same_value_does_not_lower_safety() {
        let deleting = Behavior {
            conflict_loser: ConflictLoser::Delete,
            max_delete: 80,
            versions: false,
            ..Behavior::default()
        };

        for def in &KEYS {
            assert!(!def.lowers(&deleting, &deleting), "{}", def.name);
        }
    }

    #[test]
    fn mode_lowers_safety_only_when_it_becomes_a_different_mirror() {
        let mode = def("mode");
        let at = |mode| Behavior {
            mode,
            ..Behavior::default()
        };
        let modes = [
            SyncMode::Bisync,
            SyncMode::Upload,
            SyncMode::Download,
            SyncMode::MirrorUp,
            SyncMode::MirrorDown,
        ];

        for before in modes {
            for after in modes {
                assert_eq!(
                    mode.lowers(&at(before), &at(after)),
                    after.is_mirror() && after != before,
                    "{before:?} to {after:?}"
                );
            }
        }
    }

    #[test]
    fn keys_apply_to_the_modes_of_the_spec_table() {
        let modes = [
            SyncMode::Bisync,
            SyncMode::Upload,
            SyncMode::Download,
            SyncMode::MirrorUp,
            SyncMode::MirrorDown,
        ];
        let applying = |name: &str| -> Vec<SyncMode> {
            modes
                .into_iter()
                .filter(|mode| (def(name).applies_to)(*mode))
                .collect()
        };

        for name in ["watch", "debounce", "max_wait"] {
            assert_eq!(
                applying(name),
                [SyncMode::Bisync, SyncMode::Upload, SyncMode::MirrorUp],
                "{name}"
            );
        }
        for name in ["interval", "empty_dirs", "review", "mode"] {
            assert_eq!(applying(name), modes, "{name}");
        }
        for name in ["conflict_resolve", "conflict_loser", "resync_mode"] {
            assert_eq!(applying(name), [SyncMode::Bisync], "{name}");
        }
        for name in ["versions", "versions_keep_days"] {
            assert_eq!(
                applying(name),
                [
                    SyncMode::Bisync,
                    SyncMode::Download,
                    SyncMode::MirrorUp,
                    SyncMode::MirrorDown
                ],
                "{name}"
            );
        }
        assert_eq!(
            applying("max_delete"),
            [SyncMode::Bisync, SyncMode::MirrorUp, SyncMode::MirrorDown]
        );
        assert_eq!(
            applying("limit_upload"),
            [SyncMode::Bisync, SyncMode::Upload, SyncMode::MirrorUp]
        );
        assert_eq!(
            applying("limit_download"),
            [SyncMode::Bisync, SyncMode::Download, SyncMode::MirrorDown]
        );
    }

    #[test]
    fn a_mode_is_one_of_the_five_names_and_unknown_ones_are_refused() {
        for name in ["bisync", "upload", "download", "mirror_up", "mirror_down"] {
            assert_eq!(text("mode", name).unwrap(), name);
        }
        assert_eq!(
            text("mode", "two_way").unwrap_err(),
            "unknown value \"two_way\" for mode: use bisync, upload, download, mirror_up or mirror_down"
        );
    }

    #[test]
    fn values_have_a_json_form() {
        assert_eq!(Value::Bool(true).to_json(), serde_json::json!(true));
        assert_eq!(Value::Int(50).to_json(), serde_json::json!(50));
        assert_eq!(
            Value::Duration(Duration::from_secs(300)).to_json(),
            serde_json::json!("5m")
        );
        assert_eq!(Value::Choice("newer").to_json(), serde_json::json!("newer"));
    }

    #[test]
    fn origins_are_named_for_the_cli() {
        assert_eq!(Origin::Builtin.name(), "builtin");
        assert_eq!(Origin::Defaults.name(), "defaults");
        assert_eq!(Origin::Sync.name(), "sync");
    }
}
