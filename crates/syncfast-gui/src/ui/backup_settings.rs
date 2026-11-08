//! The Settings tab of a backup: an editable draft that lives in egui
//! memory until it equals what is saved.

use std::path::PathBuf;

use egui::{Id, RichText, Ui};
use syncfast_core::backup::config::{BackupConfig, keep_reduced};

use super::filters::{self, FilterDraft, FilterRules, FilterTarget};
use super::widgets::{
    AlertButton, ButtonKind, alert, button_with_icon, icon_button, inline_error, inline_field,
    primary_button, segmented, setting_row, toggle,
};
use super::{Action, ViewModel};
use crate::icons::Icon;
use crate::snapshot::BackupSnapshot;
use crate::theme::{self, Palette, size, space};

const SCHEDULES: [(&str, &str); 3] = [
    ("hourly", "Every hour"),
    ("daily", "Every day"),
    ("weekly", "Every week"),
];
const KEEP_LABELS: [&str; 4] = ["Daily", "Weekly", "Monthly", "Yearly"];
const LIMIT_HINT: &str = "off";
const LIMIT_CAPTION: &str = "off, or a rate per second such as 500K, 2M or 1G";
const KEEP_CAPTION: &str = "How many snapshots to keep for each period; 0 keeps none.";
const SAVED: &str = "Saved.";
const FILTERS_NOTE: &str = "Files you left out stay in older snapshots until they expire.";
const KEEP_NUMBERS: &str = "The numbers to keep must be whole numbers, 0 or more.";

#[derive(Debug, Clone, PartialEq)]
struct Draft {
    paths: Vec<PathBuf>,
    every: String,
    keep: [String; 4],
    one_file_system: bool,
    upload: String,
    download: String,
    filters: FilterDraft,
}

fn limit_text(limit: &str) -> String {
    if limit == "off" {
        String::new()
    } else {
        limit.to_owned()
    }
}

fn limit_value(text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        "off".to_owned()
    } else {
        text.to_owned()
    }
}

impl Draft {
    fn from_config(config: &BackupConfig) -> Self {
        Self {
            paths: config.paths.clone(),
            every: config.every.clone(),
            keep: [
                config.keep.daily.to_string(),
                config.keep.weekly.to_string(),
                config.keep.monthly.to_string(),
                config.keep.yearly.to_string(),
            ],
            one_file_system: config.one_file_system,
            upload: limit_text(&config.limit_upload),
            download: limit_text(&config.limit_download),
            filters: FilterDraft::saved(&config.presets, &config.filters),
        }
    }

    /// `saved` with the folders and rules of the draft: what the filter
    /// test needs.
    fn with_rules(&self, saved: &BackupConfig) -> BackupConfig {
        let mut config = saved.clone();
        config.paths.clone_from(&self.paths);
        config.presets.clone_from(&self.filters.presets);
        config.filters = self.filters.custom_rules();
        config
    }

    /// The config the draft stands for.
    ///
    /// # Errors
    /// A keep field that is not a whole number of 0 or more.
    fn build(&self, saved: &BackupConfig) -> Result<BackupConfig, String> {
        let mut config = self.with_rules(saved);
        let mut keep = [0_i64; 4];
        for (slot, text) in keep.iter_mut().zip(&self.keep) {
            *slot = text
                .trim()
                .parse::<u32>()
                .map(i64::from)
                .map_err(|_| KEEP_NUMBERS.to_owned())?;
        }
        config.keep.daily = keep[0];
        config.keep.weekly = keep[1];
        config.keep.monthly = keep[2];
        config.keep.yearly = keep[3];
        config.every.clone_from(&self.every);
        config.one_file_system = self.one_file_system;
        config.limit_upload = limit_value(&self.upload);
        config.limit_download = limit_value(&self.download);
        Ok(config)
    }
}

fn flag(ctx: &egui::Context, key: Id) -> bool {
    ctx.data(|data| data.get_temp::<bool>(key)).unwrap_or(false)
}

fn set_flag(ctx: &egui::Context, key: Id, on: bool) {
    ctx.data_mut(|data| {
        if on {
            data.insert_temp(key, true);
        } else {
            data.remove::<bool>(key);
        }
    });
}

fn section(ui: &mut Ui, title: &str, palette: &Palette) {
    ui.add_space(space::L);
    ui.label(
        RichText::new(title)
            .font(theme::headline())
            .color(palette.text),
    );
}

fn note(ui: &mut Ui, text: &str, palette: &Palette) {
    ui.label(
        RichText::new(text)
            .font(theme::callout())
            .color(palette.secondary),
    );
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn show(ui: &mut Ui, vm: &ViewModel, backup: &BackupSnapshot) -> Vec<Action> {
    let ctx = ui.ctx().clone();
    let palette = theme::palette(&ctx);
    let saved = &backup.config;
    let id = saved.id.as_str();
    let saved_draft = Draft::from_config(saved);
    let draft_key = Id::new(("backup-settings-draft", id));
    let keep_key = Id::new(("backup-keep-alert", id));
    let one_fs_key = Id::new(("backup-one-fs-alert", id));
    let mut draft = ctx
        .data(|data| data.get_temp::<Draft>(draft_key))
        .unwrap_or_else(|| saved_draft.clone());
    let mut actions = Vec::new();

    if let Some(path) = &vm.backup.picked {
        if !draft.paths.contains(path) {
            draft.paths.push(path.clone());
        }
        actions.push(Action::TakeBackupFolder);
    }

    ui.spacing_mut().item_spacing.y = space::S;
    folders(ui, &mut draft, &palette, &mut actions);

    section(ui, "Destination", &palette);
    note(ui, &saved.dest, &palette);

    section(ui, "Schedule", &palette);
    let labels: Vec<&str> = SCHEDULES.iter().map(|(_, label)| *label).collect();
    let current = SCHEDULES
        .iter()
        .position(|(value, _)| *value == draft.every)
        .unwrap_or(usize::MAX);
    if let Some(index) = segmented(ui, current, &labels) {
        SCHEDULES[index].0.clone_into(&mut draft.every);
    }

    section(ui, "Keep", &palette);
    note(ui, KEEP_CAPTION, &palette);
    for (label, value) in KEEP_LABELS.iter().zip(draft.keep.iter_mut()) {
        setting_row(ui, label, None, |ui| {
            inline_field(ui, label, value, "0", size::SETTING_FIELD);
        });
    }

    section(ui, "Options", &palette);
    options(ui, &mut draft, saved, one_fs_key);

    let built = draft.build(saved);
    let problem = match &built {
        Err(text) => Some(text.clone()),
        Ok(config) => config.behavior().err(),
    };
    let rules_ok = draft.filters.problems_for(FilterTarget::Backup).is_empty();
    let dirty = !built.as_ref().is_ok_and(|config| config == saved);
    let can_save = dirty && problem.is_none() && rules_ok;
    if let Some(text) = &problem {
        ui.add_space(space::S);
        inline_error(ui, text);
    }

    section(ui, "Filters", &palette);
    let current_rules = FilterRules {
        presets: draft.filters.presets.clone(),
        custom: draft.filters.custom_rules(),
    };
    let tested = vm
        .backup
        .filter_test
        .as_ref()
        .filter(|test| test.id == id && test.rules == current_rules);
    let mut save_clicked = false;
    let test_clicked = filters::form_for(
        ui,
        id,
        &mut draft.filters,
        tested,
        FilterTarget::Backup,
        |ui| {
            ui.add_enabled_ui(can_save, |ui| {
                save_clicked = primary_button(ui, "Save").clicked();
            });
        },
    );
    if test_clicked {
        actions.push(Action::TestBackupFilters {
            backup: Box::new(draft.with_rules(saved)),
        });
    }

    let filters_changed = !draft
        .filters
        .same_rules(&FilterDraft::saved(&saved.presets, &saved.filters));
    let save = |config: BackupConfig| Action::SaveBackupSettings {
        backup: Box::new(config),
        filters_changed,
    };
    if save_clicked && let Ok(config) = &built {
        if keep_reduced(saved, config).is_empty() {
            actions.push(save(config.clone()));
        } else {
            set_flag(&ctx, keep_key, true);
        }
    }

    let dirty_now = !draft.build(saved).is_ok_and(|config| config == *saved);
    match vm.backup.saved {
        Some(_) if dirty_now => actions.push(Action::DismissBackupSaved(id.to_owned())),
        Some(changed) => {
            note(ui, SAVED, &palette);
            if changed {
                note(ui, FILTERS_NOTE, &palette);
            }
        }
        None => {}
    }

    keep_alert(&ctx, saved, &draft, keep_key, &mut actions, save);
    one_file_system_alert(&ctx, &mut draft, one_fs_key);

    ctx.data_mut(|data| {
        if draft == saved_draft {
            data.remove::<Draft>(draft_key);
        } else {
            data.insert_temp(draft_key, draft);
        }
    });
    actions
}

fn folders(ui: &mut Ui, draft: &mut Draft, palette: &Palette, actions: &mut Vec<Action>) {
    section(ui, "Folders", palette);
    let mut remove = None;
    for (index, folder) in draft.paths.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add(Icon::Folder.image(palette.secondary, size::ICON));
            ui.label(
                RichText::new(folder.display().to_string())
                    .font(theme::body())
                    .color(palette.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, Icon::X, "Remove Folder").clicked() {
                    remove = Some(index);
                }
            });
        });
        if folder.is_absolute() && !folder.exists() {
            ui.label(
                RichText::new("This folder is missing.")
                    .font(theme::callout())
                    .color(palette.warning),
            );
        }
    }
    if let Some(index) = remove {
        draft.paths.remove(index);
    }
    if button_with_icon(
        ui,
        ButtonKind::Secondary,
        Some(Icon::Plus),
        "Add Folder\u{2026}",
    )
    .clicked()
    {
        actions.push(Action::PickBackupFolder);
    }
}

fn options(ui: &mut Ui, draft: &mut Draft, saved: &BackupConfig, key: Id) {
    let mut on = draft.one_file_system;
    let changed = setting_row(ui, "Stay on one file system", None, |ui| {
        toggle(ui, &mut on, "Stay on one file system").changed()
    });
    if changed {
        if !on && saved.one_file_system {
            set_flag(ui.ctx(), key, true);
        } else {
            draft.one_file_system = on;
        }
    }
    setting_row(ui, "Upload limit", Some(LIMIT_CAPTION), |ui| {
        inline_field(
            ui,
            "Upload limit",
            &mut draft.upload,
            LIMIT_HINT,
            size::SETTING_FIELD,
        );
    });
    setting_row(ui, "Download limit", Some(LIMIT_CAPTION), |ui| {
        inline_field(
            ui,
            "Download limit",
            &mut draft.download,
            LIMIT_HINT,
            size::SETTING_FIELD,
        );
    });
}

fn keep_alert(
    ctx: &egui::Context,
    saved: &BackupConfig,
    draft: &Draft,
    key: Id,
    actions: &mut Vec<Action>,
    save: impl FnOnce(BackupConfig) -> Action,
) {
    if !flag(ctx, key) {
        return;
    }
    let text = format!(
        "The next backup permanently deletes the snapshots outside the new policy from {}.",
        saved.dest
    );
    let chosen = alert(
        ctx,
        "backup-keep-alert",
        "Delete older snapshots?",
        &text,
        &[
            AlertButton::cancel("Cancel"),
            AlertButton::destructive("Save and Delete Old Snapshots"),
        ],
        |_| {},
    );
    if let Some(index) = chosen {
        set_flag(ctx, key, false);
        if index == 1
            && let Ok(config) = draft.build(saved)
        {
            actions.push(save(config));
        }
    }
}

fn one_file_system_alert(ctx: &egui::Context, draft: &mut Draft, key: Id) {
    if !flag(ctx, key) {
        return;
    }
    let chosen = alert(
        ctx,
        "backup-one-fs-alert",
        "Include mounted drives and subvolumes?",
        "Folders mounted inside these folders, including Btrfs subvolumes and rclone mounts, are backed up too.",
        &[
            AlertButton::cancel("Cancel"),
            AlertButton::normal("Turn Off"),
        ],
        |_| {},
    );
    if let Some(index) = chosen {
        set_flag(ctx, key, false);
        if index == 1 {
            draft.one_file_system = false;
        }
    }
}
