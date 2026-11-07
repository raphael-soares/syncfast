//! The rows of the settings registry, drawn the same way in the Options tab
//! of a sync and in Settings > Defaults: one `setting_row` per key, grouped
//! in sections. A number or duration is committed when the field loses focus
//! and only if it is valid; a refused text stays in the field with the
//! reason right below the row.

use egui::{Id, RichText, Ui, WidgetInfo, WidgetType};
use syncfast_core::behavior::Behavior;
use syncfast_core::config::SettingScope;
use syncfast_core::settings::{self, Access, KeyDef, Origin, Value};

use super::widgets::{icon_button, inline_error, inline_field, setting_row, toggle};
use super::{Action, ViewModel};
use crate::icons::Icon;
use crate::theme::{self, size, space};

const SECTIONS: [(&str, &[&str]); 5] = [
    ("Schedule", &["watch", "debounce", "max_wait", "interval"]),
    ("Conflicts", &["conflict_resolve", "conflict_loser"]),
    ("Versions", &["versions", "versions_keep_days"]),
    ("Deletions", &["max_delete"]),
    ("Advanced", &["resync_mode", "empty_dirs"]),
];

/// What one row shows.
struct Row {
    value: Value,
    /// The resolved settings of the scope, to judge what one key does to
    /// another.
    behavior: Behavior,
    caption: String,
    /// `Reset to Default` is offered: the key is set in this scope.
    resettable: bool,
}

/// The text the user is typing in a duration or number field, and why it was
/// refused.
#[derive(Clone)]
struct Draft {
    text: String,
    error: Option<String>,
}

/// The sections of the selected sync.
#[must_use]
pub fn sync(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let Some(pair) = vm.selected_pair() else {
        return Vec::new();
    };
    let scope = SettingScope::Sync(pair.id.clone());
    sections(ui, &scope, |def| {
        let inherited = value_label(def.name, &def.get(&vm.defaults));
        let overridden = pair.effective.origin(def.name) == Origin::Sync;
        Row {
            value: def.get(&pair.effective.behavior),
            behavior: pair.effective.behavior.clone(),
            caption: if overridden {
                format!("Overrides the default ({inherited})")
            } else {
                format!("Default: {inherited}")
            },
            resettable: overridden,
        }
    })
}

/// The sections of `[defaults]`.
#[must_use]
pub fn defaults(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    sections(ui, &SettingScope::Defaults, |def| {
        let set = vm.defaults_overrides.contains(def.name);
        let overriding = vm
            .pairs
            .iter()
            .filter(|pair| pair.effective.origin(def.name) == Origin::Sync)
            .count();
        let mut notes = Vec::new();
        if set {
            let builtin = value_label(def.name, &def.builtin());
            notes.push(format!("Built-in default: {builtin}"));
        }
        match overriding {
            0 => {}
            1 => notes.push("Overridden by 1 sync".to_owned()),
            count => notes.push(format!("Overridden by {count} syncs")),
        }
        Row {
            value: def.get(&vm.defaults),
            behavior: vm.defaults.clone(),
            caption: notes.join(" \u{b7} "),
            resettable: set,
        }
    })
}

fn sections(ui: &mut Ui, scope: &SettingScope, row: impl Fn(&KeyDef) -> Row) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    let mut first = true;
    for (title, keys) in SECTIONS {
        let defs: Vec<&KeyDef> = keys
            .iter()
            .filter_map(|name| settings::key(name))
            .filter(|def| !(def.per_sync_only && *scope == SettingScope::Defaults))
            .collect();
        if defs.is_empty() {
            continue;
        }
        if !first {
            ui.add_space(space::XXL);
        }
        first = false;
        ui.label(
            RichText::new(title)
                .font(theme::headline())
                .color(palette.text),
        );
        for def in defs {
            setting(ui, scope, def, &row(def), &mut actions);
        }
    }
    actions
}

fn setting(ui: &mut Ui, scope: &SettingScope, def: &KeyDef, row: &Row, actions: &mut Vec<Action>) {
    let title = label(def.name);
    let draft_id = Id::new(("setting-draft", scope_name(scope), def.name));
    let caption = (!row.caption.is_empty()).then_some(row.caption.as_str());
    let error = setting_row(ui, title, caption, |ui| {
        let error = control(ui, scope, def, row, draft_id, actions);
        if row.resettable && icon_button(ui, Icon::RotateCcw, "Reset to Default").clicked() {
            ui.data_mut(|data| data.remove::<Draft>(draft_id));
            actions.push(Action::ResetSetting {
                scope: scope.clone(),
                key: def.name.to_owned(),
            });
        }
        error
    });
    if let Some(message) = error {
        inline_error(ui, &message);
    }
}

fn control(
    ui: &mut Ui,
    scope: &SettingScope,
    def: &KeyDef,
    row: &Row,
    draft_id: Id,
    actions: &mut Vec<Action>,
) -> Option<String> {
    let title = label(def.name);
    let set = |actions: &mut Vec<Action>, value: &Value| {
        actions.push(Action::SetSetting {
            scope: scope.clone(),
            key: def.name.to_owned(),
            value: value.to_string(),
        });
    };
    match (&def.access, &row.value) {
        (Access::Bool { .. }, Value::Bool(current)) => {
            let mut on = *current;
            if toggle(ui, &mut on, title).changed() {
                set(actions, &Value::Bool(on));
            }
            None
        }
        (Access::Choice { names, .. }, Value::Choice(current)) => {
            let mut error = ui
                .data(|data| data.get_temp::<Draft>(draft_id))
                .and_then(|draft| draft.error);
            let shown = choice_label(def.name, current);
            let combo = egui::ComboBox::from_id_salt(draft_id.with("combo"))
                .selected_text(shown)
                .width(size::SETTING_COMBO)
                .show_ui(ui, |ui| {
                    for name in *names {
                        let selected = name == current;
                        if !ui
                            .selectable_label(selected, choice_label(def.name, name))
                            .clicked()
                            || selected
                        {
                            continue;
                        }
                        let value = Value::Choice(name);
                        if let Some(message) = refusal(row, def, &value) {
                            let draft = Draft {
                                text: String::new(),
                                error: Some(message.clone()),
                            };
                            ui.data_mut(|data| data.insert_temp(draft_id, draft));
                            error = Some(message);
                        } else {
                            ui.data_mut(|data| data.remove::<Draft>(draft_id));
                            error = None;
                            set(actions, &value);
                        }
                    }
                });
            combo.response.widget_info(|| {
                let mut info = WidgetInfo::new(WidgetType::ComboBox);
                info.label = Some(title.to_owned());
                info.current_text_value = Some(shown.to_owned());
                info
            });
            error
        }
        _ => text_control(ui, scope, def, row, draft_id, actions),
    }
}

/// The problem between keys that setting `def` to `value` would cause, which
/// the config refuses to write.
fn refusal(row: &Row, def: &KeyDef, value: &Value) -> Option<String> {
    let mut next = row.behavior.clone();
    def.apply(&mut next, value);
    settings::relation_problem(&next)
        .filter(|problem| settings::relation_problem(&row.behavior).as_ref() != Some(problem))
}

fn text_control(
    ui: &mut Ui,
    scope: &SettingScope,
    def: &KeyDef,
    row: &Row,
    draft_id: Id,
    actions: &mut Vec<Action>,
) -> Option<String> {
    let title = label(def.name);
    if let Some(unit) = suffix(def.name) {
        let palette = theme::palette(ui.ctx());
        ui.label(
            RichText::new(unit)
                .font(theme::body())
                .color(palette.secondary),
        );
    }
    let draft = ui.data(|data| data.get_temp::<Draft>(draft_id));
    let mut text = draft
        .as_ref()
        .map_or_else(|| row.value.to_string(), |draft| draft.text.clone());
    let mut error = draft.and_then(|draft| draft.error);
    let response = inline_field(ui, title, &mut text, size::SETTING_FIELD);
    if response.changed() {
        error = None;
        let draft = Draft {
            text: text.clone(),
            error: None,
        };
        ui.data_mut(|data| data.insert_temp(draft_id, draft));
    }
    if response.lost_focus() {
        let parsed = def
            .parse_text(text.trim())
            .and_then(|value| refusal(row, def, &value).map_or(Ok(value), Err));
        match parsed {
            Ok(value) => {
                ui.data_mut(|data| data.remove::<Draft>(draft_id));
                error = None;
                if value != row.value {
                    actions.push(Action::SetSetting {
                        scope: scope.clone(),
                        key: def.name.to_owned(),
                        value: value.to_string(),
                    });
                }
            }
            Err(message) => {
                error = Some(message.clone());
                let draft = Draft {
                    text,
                    error: Some(message),
                };
                ui.data_mut(|data| data.insert_temp(draft_id, draft));
            }
        }
    }
    error
}

fn scope_name(scope: &SettingScope) -> &str {
    match scope {
        SettingScope::Defaults => "",
        SettingScope::Sync(id) => id,
    }
}

fn label(key: &str) -> &'static str {
    match key {
        "watch" => "Watch for changes",
        "debounce" => "Wait after changes",
        "max_wait" => "Longest wait while editing",
        "interval" => "Check Drive every",
        "conflict_resolve" => "When both sides change",
        "conflict_loser" => "The losing version",
        "versions" => "Keep replaced files",
        "versions_keep_days" => "Keep for",
        "max_delete" => "Stop when more than this share would be deleted",
        "resync_mode" => "When a first sync finds different files",
        "empty_dirs" => "Sync empty folders",
        _ => "",
    }
}

fn choice_label<'a>(key: &str, name: &'a str) -> &'a str {
    match (key, name) {
        ("conflict_resolve", "none") => "Keep both",
        ("conflict_loser", "num") => "Rename as .conflictN",
        ("conflict_loser", "pathname") => "Rename as .path1 or .path2",
        ("conflict_loser", "delete") => "Delete",
        (_, "newer") => "Newer wins",
        (_, "older") => "Older wins",
        (_, "larger") => "Larger wins",
        (_, "smaller") => "Smaller wins",
        (_, "local") => "This computer wins",
        (_, "remote") => "Drive wins",
        _ => name,
    }
}

/// The unit a number field is typed in, shown next to it.
fn suffix(key: &str) -> Option<&'static str> {
    match key {
        "versions_keep_days" => Some("days"),
        "max_delete" => Some("%"),
        _ => None,
    }
}

/// A value as a caption says it: `5 min`, `30 days`, `On`, `Newer wins`.
fn value_label(key: &str, value: &Value) -> String {
    match value {
        Value::Bool(true) => "On".to_owned(),
        Value::Bool(false) => "Off".to_owned(),
        Value::Duration(duration) => duration_label(duration.as_secs()),
        Value::Int(1) if key == "versions_keep_days" => "1 day".to_owned(),
        Value::Int(count) if key == "versions_keep_days" => format!("{count} days"),
        Value::Int(count) => format!("{count}%"),
        Value::Choice(name) => choice_label(key, name).to_owned(),
    }
}

fn duration_label(seconds: u64) -> String {
    let parts = [
        (seconds / 3600, "h"),
        (seconds % 3600 / 60, "min"),
        (seconds % 60, "s"),
    ];
    let shown: Vec<String> = parts
        .into_iter()
        .filter(|(amount, _)| *amount > 0)
        .map(|(amount, unit)| format!("{amount} {unit}"))
        .collect();
    if shown.is_empty() {
        "0 s".to_owned()
    } else {
        shown.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_a_label_and_a_place_in_a_section() {
        for def in &settings::KEYS {
            assert!(!label(def.name).is_empty(), "{} has no label", def.name);
            let places = SECTIONS
                .iter()
                .filter(|(_, keys)| keys.contains(&def.name))
                .count();
            assert_eq!(places, 1, "{} is in {places} sections", def.name);
        }
    }

    #[test]
    fn every_choice_has_its_own_label() {
        for def in &settings::KEYS {
            if let Access::Choice { names, .. } = &def.access {
                for name in *names {
                    assert_ne!(choice_label(def.name, name), *name, "{}:{name}", def.name);
                }
            }
        }
    }

    #[test]
    fn durations_read_as_hours_minutes_and_seconds() {
        assert_eq!(duration_label(30), "30 s");
        assert_eq!(duration_label(300), "5 min");
        assert_eq!(duration_label(5400), "1 h 30 min");
        assert_eq!(duration_label(0), "0 s");
    }

    #[test]
    fn numbers_carry_their_unit() {
        assert_eq!(
            value_label("versions_keep_days", &Value::Int(30)),
            "30 days"
        );
        assert_eq!(value_label("versions_keep_days", &Value::Int(1)), "1 day");
        assert_eq!(value_label("max_delete", &Value::Int(50)), "50%");
    }
}
