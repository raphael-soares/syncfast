//! The sheets and alerts around the backup key: the key itself, the question
//! before it is shown, entering it again, and removing a backup.

use std::path::PathBuf;

use egui::{Id, RichText, Ui};
use syncfast_core::backup::key::Key;
use zeroize::Zeroizing;

use super::Action;
use super::backup_wizard::BackupSheetAction;
use super::widgets::{self, AlertButton, PrimaryKind, SheetAction, SheetButtons, alert, sheet};
use crate::theme;

const KEY_INTRO: &str = "This key opens your backup. syncfast cannot recover it, and without it the backup cannot be read. Keep it somewhere safe.";
const FILE_NOTE: &str = "The file is not encrypted.";

/// How `Save Key to File…` ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFileOutcome {
    Saved(PathBuf),
    Failed(String),
}

fn emit(actions: &mut Vec<Action>, action: BackupSheetAction) {
    actions.push(Action::BackupSheet(action));
}

/// The key to write down, with the way to save it to a file.
pub fn key_body(ui: &mut Ui, key: &Key, file: Option<&KeyFileOutcome>, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(KEY_INTRO)
            .font(theme::callout())
            .color(palette.secondary),
    );
    let groups = Zeroizing::new(key.display_groups());
    ui.add(
        egui::Label::new(
            RichText::new(groups.as_str())
                .font(theme::monospace())
                .color(palette.text),
        )
        .selectable(true),
    );
    if widgets::secondary_button(ui, "Save Key to File\u{2026}").clicked() {
        emit(actions, BackupSheetAction::SaveKeyToFile(key.clone()));
    }
    ui.label(
        RichText::new(FILE_NOTE)
            .font(theme::footnote())
            .color(palette.secondary),
    );
    match file {
        Some(KeyFileOutcome::Saved(path)) => {
            ui.label(
                RichText::new(format!("Saved to {}.", path.display()))
                    .font(theme::footnote())
                    .color(palette.secondary),
            );
        }
        Some(KeyFileOutcome::Failed(text)) => {
            widgets::inline_error(ui, text);
        }
        None => {}
    }
}

#[derive(Debug, Clone, Copy)]
pub struct KeyView<'a> {
    pub key: Option<&'a Key>,
    /// The key was never saved: the sheet asks for `I Saved the Key`.
    pub unsaved: bool,
    pub error: Option<&'a str>,
    pub file: Option<&'a KeyFileOutcome>,
}

/// The key of an existing backup, with `Done`, or with `I Saved the Key`
/// when it was never saved.
#[must_use]
pub fn show_key(ctx: &egui::Context, view: &KeyView<'_>) -> Vec<Action> {
    let buttons = SheetButtons {
        primary: if view.unsaved {
            "I Saved the Key"
        } else {
            "Done"
        },
        primary_kind: PrimaryKind::Normal,
        primary_enabled: view.key.is_some(),
        back: false,
    };
    let mut actions = Vec::new();
    let chosen = sheet(ctx, "backup-key", "Your Backup Key", &buttons, |ui| {
        if let Some(key) = view.key {
            key_body(ui, key, view.file, &mut actions);
        }
        if let Some(error) = view.error {
            widgets::inline_error(ui, error);
        }
    });
    match chosen {
        Some(SheetAction::Cancel) => emit(&mut actions, BackupSheetAction::Cancel),
        Some(SheetAction::Primary) if view.key.is_some() => {
            let action = if view.unsaved {
                BackupSheetAction::MarkKeySaved
            } else {
                BackupSheetAction::Cancel
            };
            emit(&mut actions, action);
        }
        _ => {}
    }
    actions
}

/// The question before a key is shown.
#[must_use]
pub fn show_key_alert(ctx: &egui::Context) -> Vec<Action> {
    let chosen = alert(
        ctx,
        "show-backup-key",
        "Show the backup key?",
        "Anyone who sees this key can read your backed-up files.",
        &[
            AlertButton::cancel("Cancel"),
            AlertButton::normal("Show Key"),
        ],
        |_| {},
    );
    match chosen {
        Some(0) => vec![Action::BackupSheet(BackupSheetAction::Cancel)],
        Some(1) => vec![Action::BackupSheet(BackupSheetAction::ConfirmShowKey)],
        _ => Vec::new(),
    }
}

#[derive(Debug)]
pub struct EnterKeyView<'a> {
    pub error: Option<&'a str>,
    pub busy: bool,
}

/// Asks for the key of a backup whose key is missing or wrong. `typed` is
/// the text field; it is the caller's so it can be zeroed.
#[must_use]
pub fn enter_key(
    ctx: &egui::Context,
    view: &EnterKeyView<'_>,
    typed: &mut super::backup_wizard::TypedKey,
    show: &mut bool,
) -> Vec<Action> {
    let parsed = typed.parse();
    let problem = match (&parsed, typed.is_empty()) {
        (Err(err), false) => Some(err.to_string()),
        _ => None,
    };
    let buttons = SheetButtons {
        primary: "Save Key",
        primary_kind: PrimaryKind::Normal,
        primary_enabled: parsed.is_ok() && !view.busy,
        back: false,
    };
    let mut actions = Vec::new();
    let chosen = sheet(ctx, "enter-backup-key", "Enter Key", &buttons, |ui| {
        let changed = if *show {
            widgets::text_field(ui, "Backup key", typed.buffer(), "").changed()
        } else {
            widgets::secret_field(ui, "Backup key", typed.buffer()).changed()
        };
        if changed {
            ui.ctx().request_repaint();
        }
        widgets::switch(ui, show, "Show");
        if let Some(text) = problem.as_deref().or(view.error) {
            widgets::inline_error(ui, text);
        }
    });
    match chosen {
        Some(SheetAction::Cancel) => emit(&mut actions, BackupSheetAction::Cancel),
        Some(SheetAction::Primary) if !view.busy => {
            if let Ok(key) = parsed {
                emit(&mut actions, BackupSheetAction::SaveEnteredKey(key));
            }
        }
        _ => {}
    }
    actions
}

#[derive(Debug, Clone, Copy)]
pub struct RemoveView<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub dest: &'a str,
}

/// The question before a backup is removed. Cancel is the default; the
/// backup stays at its destination either way.
#[must_use]
pub fn remove(ctx: &egui::Context, view: &RemoveView<'_>) -> Vec<Action> {
    let checkbox = Id::new(("remove-backup-key", view.id));
    let mut delete_key = ctx
        .data(|data| data.get_temp::<bool>(checkbox))
        .unwrap_or(false);
    let chosen = alert(
        ctx,
        "remove-backup",
        &format!("Remove \"{}\"?", view.name),
        &format!(
            "syncfast will stop backing up these folders. The backup stays at {} and can be added again with Add Existing Backup\u{2026}.",
            view.dest
        ),
        &[
            AlertButton::cancel("Cancel"),
            AlertButton::destructive("Remove"),
        ],
        |ui| {
            let outline = egui::Stroke::new(1.0, theme::palette(ctx).dim);
            ui.visuals_mut().widgets.inactive.bg_stroke = outline;
            ui.checkbox(&mut delete_key, "Also delete the key from this computer");
            if delete_key {
                ui.label(
                    RichText::new("You will need the key you saved to open this backup again.")
                        .font(theme::footnote())
                        .color(theme::palette(ctx).secondary),
                );
            }
        },
    );
    match chosen {
        Some(0) => {
            ctx.data_mut(|data| data.remove::<bool>(checkbox));
            vec![Action::BackupSheet(BackupSheetAction::Cancel)]
        }
        Some(1) => {
            ctx.data_mut(|data| data.remove::<bool>(checkbox));
            vec![Action::BackupSheet(BackupSheetAction::ConfirmRemove {
                delete_key,
            })]
        }
        _ => {
            ctx.data_mut(|data| data.insert_temp(checkbox, delete_key));
            Vec::new()
        }
    }
}
