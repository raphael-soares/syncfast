//! The registry of every behavior key: its name, type, limits and how it
//! reads and writes a [`Behavior`]. `config.toml`, the CLI, MCP and the GUI
//! all validate through it, so a value means the same thing everywhere.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::Serialize;
use toml_edit::{Item, TableLike};

use crate::behavior::{
    Behavior, ConflictLoser, ConflictResolve, ResyncMode, format_duration, parse_duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Duration(Duration),
    Int(i64),
    Choice(&'static str),
}

impl fmt::Display for Value {
    /// The canonical text: what `config.toml` stores and the CLI prints.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(value) => write!(f, "{value}"),
            Self::Duration(value) => f.write_str(&format_duration(*value)),
            Self::Int(value) => write!(f, "{value}"),
            Self::Choice(value) => f.write_str(value),
        }
    }
}

impl Value {
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Bool(value) => serde_json::Value::Bool(*value),
            Self::Int(value) => serde_json::Value::from(*value),
            Self::Duration(_) | Self::Choice(_) => serde_json::Value::String(self.to_string()),
        }
    }

    pub(crate) fn to_toml(self) -> toml_edit::Value {
        match self {
            Self::Bool(value) => toml_edit::Value::from(value),
            Self::Int(value) => toml_edit::Value::from(value),
            Self::Duration(_) | Self::Choice(_) => toml_edit::Value::from(self.to_string()),
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
}

#[derive(Debug)]
pub struct KeyDef {
    pub name: &'static str,
    /// The key is only valid in a `[[pair]]`, never in `[defaults]`.
    pub per_sync_only: bool,
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

/// In the order the docs and `settings` list them.
pub static KEYS: [KeyDef; 11] = [
    KeyDef {
        name: "watch",
        per_sync_only: false,
        access: Access::Bool {
            get: |behavior| behavior.watch,
            set: |behavior, value| behavior.watch = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "debounce",
        per_sync_only: false,
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
        access: Access::Duration {
            min: Some(Duration::from_mins(1)),
            get: |behavior| behavior.interval,
            set: |behavior, value| behavior.interval = value,
        },
        lowers_safety: None,
    },
    KeyDef {
        name: "conflict_resolve",
        per_sync_only: false,
        access: choice!(conflict_resolve, ConflictResolve),
        lowers_safety: None,
    },
    KeyDef {
        name: "conflict_loser",
        per_sync_only: false,
        access: choice!(conflict_loser, ConflictLoser),
        lowers_safety: Some(|before, after| {
            before.conflict_loser != ConflictLoser::Delete
                && after.conflict_loser == ConflictLoser::Delete
        }),
    },
    KeyDef {
        name: "versions",
        per_sync_only: false,
        access: Access::Bool {
            get: |behavior| behavior.versions,
            set: |behavior, value| behavior.versions = value,
        },
        lowers_safety: Some(|before, after| before.versions && !after.versions),
    },
    KeyDef {
        name: "versions_keep_days",
        per_sync_only: false,
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
        access: choice!(resync_mode, ResyncMode),
        lowers_safety: None,
    },
    KeyDef {
        name: "empty_dirs",
        per_sync_only: false,
        access: Access::Bool {
            get: |behavior| behavior.empty_dirs,
            set: |behavior, value| behavior.empty_dirs = value,
        },
        lowers_safety: None,
    },
];

#[must_use]
pub fn key(name: &str) -> Option<&'static KeyDef> {
    KEYS.iter().find(|def| def.name == name)
}

#[must_use]
pub fn per_sync_only_message(key: &str) -> String {
    format!("{key} is set per sync, not in [defaults]")
}

/// Problems that only exist between keys, judged on a resolved behavior.
#[must_use]
pub fn relation_problem(behavior: &Behavior) -> Option<String> {
    if behavior.max_wait < behavior.debounce {
        return Some("max_wait must be at least debounce".to_owned());
    }
    if behavior.conflict_resolve == ConflictResolve::None
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
        .filter(|def| def.lowers(&builtin, behavior))
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
        }
    }

    pub fn apply(&self, behavior: &mut Behavior, value: &Value) {
        match (&self.access, value) {
            (Access::Bool { set, .. }, Value::Bool(value)) => set(behavior, *value),
            (Access::Duration { set, .. }, Value::Duration(value)) => set(behavior, *value),
            (Access::Int { set, .. }, Value::Int(value)) => set(behavior, *value),
            (Access::Choice { set, .. }, Value::Choice(value)) => set(behavior, value),
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
        }
    }

    fn type_error(&self) -> String {
        let noun = match self.access {
            Access::Bool { .. } => "bool",
            Access::Duration { .. } => "a duration",
            Access::Int { .. } | Access::Choice { .. } => "a whole number",
        };
        format!("{} must be {noun}", self.name)
    }

    fn check_duration(&self, duration: Duration) -> Result<Value, String> {
        if let Access::Duration { min: Some(min), .. } = self.access
            && duration < min
        {
            return Err(format!(
                "{} must be at least {}",
                self.name,
                format_duration(min)
            ));
        }
        Ok(Value::Duration(duration))
    }

    fn check_int(&self, number: i64) -> Result<Value, String> {
        if let Access::Int { min, max, .. } = self.access
            && !(min..=max).contains(&number)
        {
            return Err(format!("{} must be between {min} and {max}", self.name));
        }
        Ok(Value::Int(number))
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
                    self.name,
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
        for def in &KEYS {
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
                "conflict_resolve=newer",
                "conflict_loser=num",
                "versions=true",
                "versions_keep_days=30",
                "max_delete=50",
                "resync_mode=newer",
                "empty_dirs=true",
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
            ("conflict_resolve", "local"),
            ("conflict_loser", "pathname"),
            ("versions", "false"),
            ("versions_keep_days", "7"),
            ("max_delete", "10"),
            ("resync_mode", "remote"),
            ("empty_dirs", "false"),
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
                conflict_resolve: ConflictResolve::Local,
                conflict_loser: ConflictLoser::Pathname,
                versions: false,
                versions_keep_days: 7,
                max_delete: 10,
                resync_mode: ResyncMode::Remote,
                empty_dirs: false,
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
    fn only_the_three_safety_keys_can_lower_safety() {
        let names: Vec<_> = KEYS
            .iter()
            .filter(|def| def.lowers_safety.is_some())
            .map(|def| def.name)
            .collect();

        assert_eq!(names, ["conflict_loser", "versions", "max_delete"]);
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
