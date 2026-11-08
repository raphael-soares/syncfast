//! The sheets and alerts around the backup key: the key itself, the question
//! before it is shown, entering it again, and removing a backup.

use std::path::PathBuf;

use egui::text::{LayoutJob, TextFormat};
use egui::{FontId, Id, RichText, Ui};
use syncfast_core::backup::key::Key;
use zeroize::Zeroizing;

use super::Action;
use super::backup_wizard::BackupSheetAction;
use super::widgets::{self, AlertButton, PrimaryKind, SheetAction, SheetButtons, alert, sheet};
use crate::theme::{self, radius, space};

const KEY_INTRO: &str = "This key opens your backup. syncfast cannot recover it, and without it the backup cannot be read. Keep it somewhere safe.";
const FILE_NOTE: &str = "The file is not encrypted.";
const KEY_FONT_SIZE: f32 = 16.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyFileOutcome {
    Saved(PathBuf),
    Failed(String),
}

fn emit(actions: &mut Vec<Action>, action: BackupSheetAction) {
    actions.push(Action::BackupSheet(action));
}

pub(super) fn wrap_labels(ui: &mut Ui) {
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
}

pub fn key_body(ui: &mut Ui, key: &Key, file: Option<&KeyFileOutcome>, actions: &mut Vec<Action>) {
    let palette = theme::palette(ui.ctx());
    ui.label(
        RichText::new(KEY_INTRO)
            .font(theme::callout())
            .color(palette.text),
    );
    key_box(ui, key);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::M;
        if widgets::secondary_button(ui, "Save Key to File\u{2026}").clicked() {
            emit(actions, BackupSheetAction::SaveKeyToFile(key.clone()));
        }
        ui.label(
            RichText::new(FILE_NOTE)
                .font(theme::footnote())
                .color(palette.secondary),
        );
    });
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

fn key_box(ui: &mut Ui, key: &Key) {
    let palette = theme::palette(ui.ctx());
    let groups = Zeroizing::new(key.display_groups());
    let last = groups.split('-').count().saturating_sub(1);
    let font = FontId::monospace(KEY_FONT_SIZE);
    egui::Frame::new()
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.outline))
        .corner_radius(radius::CARD)
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(space::XS, space::S);
                for (index, group) in groups.split('-').enumerate() {
                    let mut job = LayoutJob::default();
                    job.append(group, 0.0, TextFormat::simple(font.clone(), palette.text));
                    if index < last {
                        job.append("-", 0.0, TextFormat::simple(font.clone(), palette.dim));
                    }
                    // `extend`: a wrapped label would otherwise split a group across rows
                    ui.add(egui::Label::new(job).extend().selectable(true));
                }
            });
        });
}

#[derive(Debug, Clone, Copy)]
pub struct KeyView<'a> {
    pub key: Option<&'a Key>,
    /// The key was never saved: the sheet asks for `I Saved the Key`.
    pub unsaved: bool,
    pub error: Option<&'a str>,
    pub file: Option<&'a KeyFileOutcome>,
}

/// `I Saved the Key` when the key was never saved, `Done` otherwise; only
/// the first keeps `Cancel`.
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
        cancel: view.unsaved || view.key.is_none(),
    };
    let mut actions = Vec::new();
    let chosen = sheet(ctx, "backup-key", "Your Backup Key", &buttons, |ui| {
        wrap_labels(ui);
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

/// `typed` is the caller's so it can be zeroed.
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
        cancel: true,
    };
    let mut actions = Vec::new();
    let chosen = sheet(ctx, "enter-backup-key", "Enter Key", &buttons, |ui| {
        wrap_labels(ui);
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
