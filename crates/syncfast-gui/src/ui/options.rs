//! The rows of the settings registry, drawn the same way in the Options tab
//! of a sync and in Settings > Defaults: one `setting_row` per key, grouped
//! in sections. A number or duration is committed when the field loses focus
//! and only if it is valid; a refused text stays in the field with the
//! reason right below the row.

use egui::{Id, RichText, Ui, WidgetInfo, WidgetType};
use syncfast_core::behavior::{Bandwidth, Behavior};
use syncfast_core::config::SettingScope;
use syncfast_core::settings::{self, Access, KeyDef, Origin, Value};

use super::widgets::{icon_button, inline_error, inline_field, setting_row, toggle};
use super::{Action, ViewModel};
use crate::icons::Icon;
use crate::theme::{self, size, space};

const SECTIONS: [(&str, &[&str]); 6] = [
    ("Schedule", &["watch", "debounce", "max_wait", "interval"]),
    ("Bandwidth", &["limit_upload", "limit_download"]),
    ("Conflicts", &["conflict_resolve", "conflict_loser"]),
    ("Versions", &["versions", "versions_keep_days"]),
    ("Deletions", &["max_delete"]),
    ("Advanced", &["resync_mode", "empty_dirs"]),
];

/// What one row shows.
struct Row {
    value: Value,
    /// What the key becomes when reset.
    fallback: Value,
    /// The settings a change of this key can reach.
    targets: Vec<Target>,
    caption: String,
    /// `Reset to Default` is offered: the key is set in this scope.
    resettable: bool,
}

impl Row {
    fn basis(&self) -> Vec<Behavior> {
        self.targets
            .iter()
            .map(|target| target.behavior.clone())
            .collect()
    }
}

/// A resolved setting, to judge what one key does to another. `reached` is
/// false when the change does not flow into it (a sync that sets the key
/// itself).
struct Target {
    behavior: Behavior,
    reached: bool,
}

/// What the user typed in a duration or number field, and why a change was
/// refused, with the settings the refusal was judged against.
#[derive(Clone)]
struct Draft {
    text: Option<String>,
    error: Option<String>,
    basis: Vec<Behavior>,
}

/// The sections of the selected sync.
#[must_use]
pub fn sync(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let Some(pair) = vm.selected_pair() else {
        return Vec::new();
    };
    let scope = SettingScope::Sync(pair.id.clone());
    let summary = schedule_summary(&pair.effective.behavior);
    sections(ui, &scope, Some(&summary), |def| {
        let inherited = value_label(def.name, &def.get(&vm.defaults));
        let overridden = pair.effective.origin(def.name) == Origin::Sync;
        Row {
            value: def.get(&pair.effective.behavior),
            fallback: def.get(&vm.defaults),
            targets: vec![Target {
                behavior: pair.effective.behavior.clone(),
                reached: true,
            }],
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
    sections(ui, &SettingScope::Defaults, None, |def| {
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
        let mut targets = vec![Target {
            behavior: vm.defaults.clone(),
            reached: true,
        }];
        targets.extend(vm.pairs.iter().map(|pair| Target {
            behavior: pair.effective.behavior.clone(),
            reached: pair.effective.origin(def.name) != Origin::Sync,
        }));
        Row {
            value: def.get(&vm.defaults),
            fallback: def.builtin(),
            targets,
            caption: notes.join(" \u{b7} "),
            resettable: set,
        }
    })
}

fn sections(
    ui: &mut Ui,
    scope: &SettingScope,
    summary: Option<&str>,
    row: impl Fn(&KeyDef) -> Row,
) -> Vec<Action> {
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
        if let (Some(summary), "Schedule") = (summary, title) {
            ui.label(
                RichText::new(summary)
                    .font(theme::callout())
                    .color(palette.secondary),
            );
        }
        for def in defs {
            setting(ui, scope, def, &row(def), &mut actions);
        }
    }
    actions
}

fn setting(ui: &mut Ui, scope: &SettingScope, def: &KeyDef, row: &Row, actions: &mut Vec<Action>) {
    let title = label(def.name);
    let field = Field {
        scope,
        def,
        row,
        draft_id: Id::new(("setting-draft", scope_name(scope), def.name)),
    };
    let draft = field.draft(ui);
    let caption = (!row.caption.is_empty()).then_some(row.caption.as_str());
    let error = setting_row(ui, title, caption, |ui| {
        let mut error = draft.as_ref().and_then(|draft| draft.error.clone());
        let typed = draft.and_then(|draft| draft.text);
        control(ui, &field, typed.clone(), &mut error, actions);
        if row.resettable && icon_button(ui, Icon::RotateCcw, "Reset to Default").clicked() {
            if let Some(message) = refusal(row, def, &row.fallback) {
                field.refuse(ui, typed, message, &mut error);
            } else {
                field.clear(ui);
                error = None;
                actions.push(Action::ResetSetting {
                    scope: scope.clone(),
                    key: def.name.to_owned(),
                });
            }
        }
        error
    });
    if let Some(message) = error {
        inline_error(ui, &message);
    }
}

/// One row being drawn: where its draft lives and what it can change.
struct Field<'a> {
    scope: &'a SettingScope,
    def: &'a KeyDef,
    row: &'a Row,
    draft_id: Id,
}

impl Field<'_> {
    /// The draft of the row. A refusal is dropped once the settings it was
    /// judged against have changed.
    fn draft(&self, ui: &Ui) -> Option<Draft> {
        let mut draft = ui.data(|data| data.get_temp::<Draft>(self.draft_id))?;
        let basis = self.row.basis();
        if draft.basis == basis {
            return Some(draft);
        }
        if draft.text.is_none() {
            self.clear(ui);
            return None;
        }
        draft.error = None;
        draft.basis = basis;
        ui.data_mut(|data| data.insert_temp(self.draft_id, draft.clone()));
        Some(draft)
    }

    fn clear(&self, ui: &Ui) {
        ui.data_mut(|data| data.remove::<Draft>(self.draft_id));
    }

    fn keep(&self, ui: &Ui, text: Option<String>, error: Option<String>) {
        let draft = Draft {
            text,
            error,
            basis: self.row.basis(),
        };
        ui.data_mut(|data| data.insert_temp(self.draft_id, draft));
    }

    fn refuse(&self, ui: &Ui, typed: Option<String>, message: String, error: &mut Option<String>) {
        self.keep(ui, typed, Some(message.clone()));
        *error = Some(message);
    }

    /// Asks for `value`, unless it breaks a relation between keys in any
    /// setting it reaches.
    fn propose(
        &self,
        ui: &Ui,
        value: &Value,
        typed: Option<String>,
        error: &mut Option<String>,
        actions: &mut Vec<Action>,
    ) {
        if let Some(message) = refusal(self.row, self.def, value) {
            self.refuse(ui, typed, message, error);
            return;
        }
        self.clear(ui);
        *error = None;
        if *value != self.row.value {
            actions.push(Action::SetSetting {
                scope: self.scope.clone(),
                key: self.def.name.to_owned(),
                value: value.to_string(),
            });
        }
    }
}

fn control(
    ui: &mut Ui,
    field: &Field,
    typed: Option<String>,
    error: &mut Option<String>,
    actions: &mut Vec<Action>,
) {
    let title = label(field.def.name);
    match (&field.def.access, &field.row.value) {
        (Access::Bool { .. }, Value::Bool(current)) => {
            let mut on = *current;
            if toggle(ui, &mut on, title).changed() {
                field.propose(ui, &Value::Bool(on), None, error, actions);
            }
        }
        (Access::Choice { names, .. }, Value::Choice(current)) => {
            let shown = choice_label(field.def.name, current);
            let combo = egui::ComboBox::from_id_salt(field.draft_id.with("combo"))
                .selected_text(shown)
                .width(size::SETTING_COMBO)
                .show_ui(ui, |ui| {
                    for name in *names {
                        let selected = name == current;
                        if ui
                            .selectable_label(selected, choice_label(field.def.name, name))
                            .clicked()
                            && !selected
                        {
                            field.propose(ui, &Value::Choice(name), None, error, actions);
                        }
                    }
                });
            combo.response.widget_info(|| {
                let mut info = WidgetInfo::new(WidgetType::ComboBox);
                info.label = Some(title.to_owned());
                info.current_text_value = Some(shown.to_owned());
                info
            });
        }
        _ => text_control(ui, field, typed, error, actions),
    }
}

/// The first relation between keys that setting `def` to `value` would
/// break in a setting it reaches, as the config would refuse to write it.
fn refusal(row: &Row, def: &KeyDef, value: &Value) -> Option<String> {
    row.targets
        .iter()
        .filter(|target| target.reached)
        .find_map(|target| {
            let mut next = target.behavior.clone();
            def.apply(&mut next, value);
            settings::relation_problem(&next).filter(|problem| {
                settings::relation_problem(&target.behavior).as_ref() != Some(problem)
            })
        })
}

fn text_control(
    ui: &mut Ui,
    field: &Field,
    typed: Option<String>,
    error: &mut Option<String>,
    actions: &mut Vec<Action>,
) {
    let title = label(field.def.name);
    let palette = theme::palette(ui.ctx());
    ui.allocate_ui_with_layout(
        egui::vec2(size::SETTING_UNIT, size::CONTROL_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            if let Some(unit) = suffix(field.def.name) {
                ui.label(
                    RichText::new(unit)
                        .font(theme::body())
                        .color(palette.secondary),
                );
            }
        },
    );
    let mut text = typed.unwrap_or_else(|| match field.row.value {
        Value::Bandwidth(Bandwidth::Off) => String::new(),
        value => value.to_string(),
    });
    let hint = match field.def.access {
        Access::Bandwidth { .. } => "Unlimited",
        _ => "",
    };
    let response = inline_field(ui, title, &mut text, hint, size::SETTING_FIELD);
    if response.changed() {
        *error = None;
        field.keep(ui, Some(text.clone()), None);
    }
    if response.lost_focus() {
        let input = match text.trim() {
            "" if matches!(field.def.access, Access::Bandwidth { .. }) => "off",
            input => input,
        };
        match field.def.parse_text(input) {
            Ok(value) => field.propose(ui, &value, Some(text), error, actions),
            Err(message) => field.refuse(ui, Some(text), message, error),
        }
    }
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
        "limit_upload" => "Limit uploads to",
        "limit_download" => "Limit downloads to",
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
        Value::Bandwidth(Bandwidth::Off) => "Unlimited".to_owned(),
        Value::Bandwidth(Bandwidth::KiBPerSec(kib)) => match kib {
            kib if kib % (1024 * 1024) == 0 => format!("{} GiB/s", kib / (1024 * 1024)),
            kib if kib % 1024 == 0 => format!("{} MiB/s", kib / 1024),
            kib => format!("{kib} KiB/s"),
        },
    }
}

fn schedule_summary(behavior: &Behavior) -> String {
    let interval = duration_label(behavior.interval.as_secs());
    if behavior.watch {
        let debounce = duration_label(behavior.debounce.as_secs());
        format!("Syncs every {interval} and {debounce} after you stop editing.")
    } else {
        format!("Syncs every {interval}. Changes on this computer wait for the next round.")
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

    #[test]
    fn speeds_read_in_the_largest_exact_binary_unit() {
        let label = |kib| value_label("limit_upload", &Value::Bandwidth(Bandwidth::KiBPerSec(kib)));
        assert_eq!(
            value_label("limit_upload", &Value::Bandwidth(Bandwidth::Off)),
            "Unlimited"
        );
        assert_eq!(label(500), "500 KiB/s");
        assert_eq!(label(1536), "1536 KiB/s");
        assert_eq!(label(2048), "2 MiB/s");
        assert_eq!(label(1024 * 1024), "1 GiB/s");
    }
}
