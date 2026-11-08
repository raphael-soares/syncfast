//! The detail of the selected backup: toolbar, status card and the
//! Snapshots, Activity and Settings tabs.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Frame, Id, Margin, RichText, Ui, Vec2};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use syncfast_core::backup::destination::Destination;
use syncfast_core::backup::effects::retention_message;
use syncfast_core::backup::restore_job::RestoreReport;
use syncfast_core::backup::run::KEY_ABSENT;
use syncfast_core::backup::snapshots::SnapshotView;
use syncfast_core::backup::state::CheckKind;
use syncfast_core::backup::status::{BackupProgressFile, BackupStatus};
use syncfast_core::bisync::Fault;
use syncfast_core::restic::backup_host;
use syncfast_core::state::{Gate, GateKind};

use super::backup_restore::{fraction, restore_line};
use super::backup_settings;
use super::detail::padded;
use super::format::{bytes, duration, eta, started};
use super::menu::{self, Tone};
use super::widgets::{
    AlertButton, AnyStatus, BadgeSize, ButtonKind, alert, button_with_icon, empty_state,
    icon_button, inline_error, list_row, progress_bar, secondary_button, segmented, status_badge,
    toolbar,
};
use super::{Action, ViewModel, filters};
use crate::backup_runs::{BackupRunEntry, RunKind, RunResult};
use crate::icons::Icon;
use crate::snapshot::BackupSnapshot;
use crate::theme::{self, Palette, radius, size, space};

const WHEN_WIDTH: f32 = 140.0;
const DURATION_WIDTH: f32 = 60.0;
const RESULT_WIDTH: f32 = 130.0;
const LOG_MAX_HEIGHT: f32 = 220.0;

pub const KEY_CARD_TITLE: &str = "Save the backup key";
pub const KEY_CARD_TEXT: &str = "An agent created this backup. Save its key now; without it you cannot restore on another computer.";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BackupTab {
    #[default]
    Snapshots,
    Activity,
    Settings,
}

impl BackupTab {
    pub const ALL: [Self; 3] = [Self::Snapshots, Self::Activity, Self::Settings];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Snapshots => "Snapshots",
            Self::Activity => "Activity",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum SnapshotsView {
    #[default]
    Loading,
    Loaded(Arc<[SnapshotView]>),
    Failed(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BackupDetailData {
    pub tab: BackupTab,
    pub snapshots: SnapshotsView,
    pub runs: Option<Arc<[BackupRunEntry]>>,
    pub filter_test: Option<filters::FilterTest>,
    /// `Some(filters_changed)` after a `Save` of the Settings tab.
    pub saved: Option<bool>,
    /// The folder the chooser of the Folders list answered.
    pub picked: Option<PathBuf>,
    /// The restore of this backup that runs, even if its window was closed.
    pub restoring: Option<RestoreReport>,
}

#[must_use]
pub fn detail(ui: &mut Ui, vm: &ViewModel, backup: &BackupSnapshot) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    let id = backup.config.id.as_str();
    toolbar(ui, &backup.config.name, &subtitle(backup), |ui| {
        let more = icon_button(ui, Icon::Ellipsis, "More");
        more_menu(&more, backup, &palette, &mut actions);
        pause_button(ui, backup, &mut actions);
        back_up_button(ui, backup, &mut actions);
    });
    padded(ui, |ui| {
        ui.spacing_mut().item_spacing.y = space::S;
        if backup.state.key_unsaved {
            key_card(ui, id, &palette, &mut actions);
        }
        status_card(
            ui,
            backup,
            vm.backup.restoring.as_ref(),
            vm.now,
            &palette,
            &mut actions,
        );
    });
    let tab = vm.backup.tab;
    padded(ui, |ui| {
        ui.add_space(space::L);
        let selected = BackupTab::ALL
            .iter()
            .position(|shown| *shown == tab)
            .unwrap_or(0);
        let labels: Vec<&str> = BackupTab::ALL.iter().map(|tab| tab.title()).collect();
        if let Some(index) = segmented(ui, selected, &labels) {
            actions.push(Action::SelectBackupTab(BackupTab::ALL[index]));
        }
        ui.add_space(space::S);
    });
    padded(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match tab {
                BackupTab::Snapshots => actions.extend(snapshots_tab(ui, vm, backup)),
                BackupTab::Activity => actions.extend(activity_tab(ui, vm, backup, &palette)),
                BackupTab::Settings => actions.extend(backup_settings::show(ui, vm, backup)),
            });
    });
    full_check_alert(ui.ctx(), backup, &mut actions);
    actions
}

fn subtitle(backup: &BackupSnapshot) -> String {
    let folders = backup.config.paths.len();
    let noun = if folders == 1 { "folder" } else { "folders" };
    format!("{folders} {noun} \u{2192} {}", backup.config.dest)
}

fn local_destination(backup: &BackupSnapshot) -> bool {
    matches!(
        Destination::parse(&backup.config.dest),
        Ok(Destination::Local(_))
    )
}

fn account_of(backup: &BackupSnapshot) -> Option<String> {
    match Destination::parse(&backup.config.dest) {
        Ok(Destination::Account { name, .. }) => Some(name),
        _ => None,
    }
}

/// A gate that is not `needs_attention` holds the backup, and so does a
/// damaged repository; the next attempt of any other `needs_attention` one
/// may succeed.
fn can_back_up(backup: &BackupSnapshot) -> bool {
    backup.running.is_none()
        && !backup.config.paths.is_empty()
        && backup.state.gate.as_ref().is_none_or(|gate| {
            gate.kind == GateKind::NeedsAttention && gate.fault != Some(Fault::RepoDamaged)
        })
}

fn back_up_button(ui: &mut Ui, backup: &BackupSnapshot, actions: &mut Vec<Action>) {
    let clicked = ui
        .add_enabled_ui(can_back_up(backup), |ui| {
            button_with_icon(ui, ButtonKind::Primary, Some(Icon::Refresh), "Back Up Now").clicked()
        })
        .inner;
    if clicked {
        actions.push(Action::BackUpNow(backup.config.id.clone()));
    }
}

fn pause_button(ui: &mut Ui, backup: &BackupSnapshot, actions: &mut Vec<Action>) {
    let enabled = backup.config.enabled;
    let (icon, label) = if enabled {
        (Icon::Pause, "Pause")
    } else {
        (Icon::Play, "Resume")
    };
    let can_toggle = enabled || !backup.config.paths.is_empty();
    let clicked = ui
        .add_enabled_ui(can_toggle, |ui| {
            button_with_icon(ui, ButtonKind::Secondary, Some(icon), label).clicked()
        })
        .inner;
    if clicked {
        actions.push(Action::SetBackupEnabled(backup.config.id.clone(), !enabled));
    }
}

fn full_check_key(id: &str) -> Id {
    Id::new(("backup-full-check", id))
}

fn more_menu(
    button: &egui::Response,
    backup: &BackupSnapshot,
    palette: &Palette,
    actions: &mut Vec<Action>,
) {
    let id = &backup.config.id;
    let check_tone = if backup.running.is_some() {
        Tone::Disabled
    } else {
        Tone::Normal
    };
    menu::popup(button, palette, |ui| {
        if menu::item(ui, palette, Icon::KeyRound, "Show Key…", Tone::Normal) {
            actions.push(Action::ShowBackupKey(id.clone()));
        }
        ui.separator();
        ui.label(
            RichText::new("Check Now")
                .font(theme::subheadline())
                .color(palette.dim),
        );
        if menu::item(ui, palette, Icon::ShieldCheck, "Quick Check", check_tone) {
            actions.push(Action::CheckBackup {
                id: id.clone(),
                kind: CheckKind::Quick,
            });
        }
        if menu::item(ui, palette, Icon::ShieldCheck, "Check Sample", check_tone) {
            actions.push(Action::CheckBackup {
                id: id.clone(),
                kind: CheckKind::Sample,
            });
        }
        if menu::item(ui, palette, Icon::ShieldCheck, "Full Check…", check_tone) {
            ui.data_mut(|data| data.insert_temp(full_check_key(id), true));
        }
        if local_destination(backup) {
            ui.separator();
            if menu::item(
                ui,
                palette,
                Icon::Folder,
                "Open Destination Folder",
                Tone::Normal,
            ) {
                actions.push(Action::OpenBackupDestination(id.clone()));
            }
        }
        ui.separator();
        if menu::item(ui, palette, Icon::Trash, "Remove Backup…", Tone::Danger) {
            actions.push(Action::RemoveBackup(id.clone()));
        }
    });
}

fn full_check_alert(ctx: &egui::Context, backup: &BackupSnapshot, actions: &mut Vec<Action>) {
    let key = full_check_key(&backup.config.id);
    if !ctx.data(|data| data.get_temp::<bool>(key)).unwrap_or(false) {
        return;
    }
    let text = format!(
        "This downloads everything from {} and can take hours.",
        backup.config.dest
    );
    let chosen = alert(
        ctx,
        "backup-full-check",
        "Read all backup data?",
        &text,
        &[AlertButton::cancel("Cancel"), AlertButton::normal("Check")],
        |_| {},
    );
    if let Some(index) = chosen {
        ctx.data_mut(|data| data.remove::<bool>(key));
        if index == 1 {
            actions.push(Action::CheckBackup {
                id: backup.config.id.clone(),
                kind: CheckKind::Full,
            });
        }
    }
}

fn key_card(ui: &mut Ui, id: &str, palette: &Palette, actions: &mut Vec<Action>) {
    Frame::new()
        .fill(palette.surface)
        .corner_radius(radius::CARD)
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = space::S;
            ui.horizontal(|ui| {
                ui.add(Icon::KeyRound.image(palette.accent, size::ICON));
                ui.label(
                    RichText::new(KEY_CARD_TITLE)
                        .font(theme::headline())
                        .color(palette.text),
                );
            });
            ui.label(
                RichText::new(KEY_CARD_TEXT)
                    .font(theme::callout())
                    .color(palette.secondary),
            );
            if secondary_button(ui, "Show Key…").clicked() {
                actions.push(Action::ShowBackupKey(id.to_owned()));
            }
        });
}

struct CardText {
    phrase: String,
    /// Raw text of the failure, selectable so the user can copy it.
    detail: Option<String>,
    /// The last failed clean-up of old snapshots, when the phrase is not it.
    note: Option<String>,
    button: Option<(&'static str, Action)>,
}

fn when(now: Timestamp, at: Timestamp) -> String {
    started(now, &at.to_zoned(TimeZone::system()))
}

fn plural_files(count: u64) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

fn last_backup_line(backup: &BackupSnapshot, now: Timestamp) -> String {
    let Some(at) = backup.state.last_success else {
        return "Never backed up.".to_owned();
    };
    let when = when(now, at);
    let Some(run) = &backup.state.last_run else {
        return format!("Backed up {when}.");
    };
    let summary = &run.summary;
    if summary.warnings > 0 {
        return format!(
            "Backed up {when}. {} could not be read.",
            plural_files(summary.warnings)
        );
    }
    let changed = summary.files_new + summary.files_changed;
    let noun = if changed == 1 { "file" } else { "files" };
    format!(
        "Backed up {when}. {changed} new or changed {noun}, {} added.",
        bytes(summary.data_added)
    )
}

const AUTH_SENTENCE: &str = "Google Drive refused the account. Reconnect it to continue.";
const KEY_MISSING_SENTENCE: &str = "The key for this backup is missing on this computer.";
const WRONG_KEY_SENTENCE: &str = "The saved key does not open this backup.";

fn plain(phrase: String) -> CardText {
    CardText {
        phrase,
        detail: None,
        note: None,
        button: None,
    }
}

fn needs_attention_card(backup: &BackupSnapshot, gate: &Gate) -> CardText {
    let id = backup.config.id.clone();
    let detail = Some(gate.detail.clone()).filter(|detail| !detail.is_empty());
    let named = |fallback: &str| {
        if gate.detail.is_empty() {
            fallback.to_owned()
        } else {
            gate.detail.clone()
        }
    };
    let with_detail = |phrase: String, button| CardText {
        phrase,
        detail: detail.clone(),
        note: None,
        button,
    };
    match gate.fault {
        Some(Fault::Auth | Fault::Account) => with_detail(
            AUTH_SENTENCE.to_owned(),
            account_of(backup).map(|name| ("Reconnect Account…", Action::ReconnectAccount(name))),
        ),
        Some(Fault::Quota) => with_detail("Google Drive is full.".to_owned(), None),
        Some(Fault::Permission) => with_detail(
            "No permission to the backup folder on Google Drive.".to_owned(),
            None,
        ),
        Some(Fault::RemoteMissing | Fault::RepositoryMissing) => {
            with_detail("The backup folder is missing.".to_owned(), None)
        }
        Some(Fault::DestinationMissing) => plain(format!(
            "{} is not available. Backups resume when it is back.",
            backup.config.dest
        )),
        Some(fault @ (Fault::KeyMissing | Fault::WrongKey)) => CardText {
            phrase: if fault == Fault::KeyMissing {
                KEY_MISSING_SENTENCE
            } else {
                WRONG_KEY_SENTENCE
            }
            .to_owned(),
            detail: detail.filter(|detail| detail != KEY_ABSENT),
            note: None,
            button: Some(("Enter Key…", Action::EnterBackupKey(id))),
        },
        Some(Fault::SourceMissing) => plain(format!(
            "{} is missing. Backups resume when it is back.",
            named("A folder to back up")
        )),
        Some(Fault::SourceEmpty) => plain(format!(
            "{} is empty. If it is on a drive, mount it.",
            named("A folder to back up")
        )),
        Some(Fault::RepoDamaged) => with_detail(
            "The check found damaged data. Backups are paused until you review it.".to_owned(),
            Some((
                "Check Again",
                Action::CheckBackup {
                    id,
                    kind: CheckKind::Full,
                },
            )),
        ),
        Some(Fault::LocalEmpty | Fault::RemoteEmpty) | None => {
            let cleanup = backup.state.prune_error.as_deref().map(retention_message);
            match cleanup {
                Some(line) if line == gate.detail => plain(line),
                _ => with_detail("Backups keep failing.".to_owned(), None),
            }
        }
    }
}

fn base_card_text(backup: &BackupSnapshot, now: Timestamp) -> CardText {
    if let Some(gate) = &backup.state.gate
        && backup.status == BackupStatus::Error
    {
        return match gate.kind {
            GateKind::AuthError => {
                let account = account_of(backup);
                CardText {
                    phrase: if account.is_some() {
                        AUTH_SENTENCE.to_owned()
                    } else {
                        "The account of this backup was disconnected.".to_owned()
                    },
                    button: account
                        .map(|name| ("Reconnect Account…", Action::ReconnectAccount(name))),
                    ..plain(String::new())
                }
            }
            GateKind::NeedsAttention => needs_attention_card(backup, gate),
            _ => plain(if gate.detail.is_empty() {
                "The configuration of this backup has a problem.".to_owned()
            } else {
                gate.detail.clone()
            }),
        };
    }
    plain(match backup.status {
        BackupStatus::Unavailable => format!(
            "{} is not available. Backups resume when it is back.",
            backup.config.dest
        ),
        BackupStatus::Paused if backup.config.paths.is_empty() => {
            "This backup has no folder yet. Add one with Add Folder… in Settings.".into()
        }
        BackupStatus::Paused => "This backup is paused. Resume it to back up again.".into(),
        BackupStatus::Offline => "No network. Backups resume when it is back.".into(),
        BackupStatus::Failing => format!(
            "The last {} backups failed in a row.",
            backup.state.consecutive_failures
        ),
        BackupStatus::Retrying => "The last backup failed. It will try again soon.".into(),
        BackupStatus::UpToDate | BackupStatus::SaveKey => last_backup_line(backup, now),
        BackupStatus::BackingUp | BackupStatus::Checking | BackupStatus::Error => String::new(),
    })
}

fn card_text(backup: &BackupSnapshot, now: Timestamp) -> CardText {
    let mut text = base_card_text(backup, now);
    text.note = backup
        .state
        .prune_error
        .as_deref()
        .map(retention_message)
        .filter(|line| *line != text.phrase);
    text
}

fn key_problem(backup: &BackupSnapshot) -> bool {
    backup.state.gate.as_ref().is_some_and(|gate| {
        gate.kind == GateKind::NeedsAttention
            && matches!(gate.fault, Some(Fault::KeyMissing | Fault::WrongKey))
    })
}

fn status_card(
    ui: &mut Ui,
    backup: &BackupSnapshot,
    restoring: Option<&RestoreReport>,
    now: Timestamp,
    palette: &Palette,
    actions: &mut Vec<Action>,
) {
    Frame::new()
        .fill(palette.surface)
        .corner_radius(radius::CARD)
        .inner_margin(Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = space::S;
            status_badge(ui, backup.status, BadgeSize::Large);
            if let Some(restore) = restoring {
                restoring_block(ui, restore, palette);
            }
            if backup.status == BackupStatus::BackingUp {
                progress_block(ui, backup.progress.as_ref(), palette);
                return;
            }
            if backup.status == BackupStatus::Checking {
                progress_bar(ui, "Check progress", None);
                return;
            }
            let text = card_text(backup, now);
            if !text.phrase.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(text.phrase)
                            .font(theme::callout())
                            .color(palette.secondary),
                    )
                    .selectable(false),
                );
            }
            if let Some(detail) = text.detail {
                ui.add(
                    egui::Label::new(
                        RichText::new(detail)
                            .font(theme::subheadline())
                            .color(palette.secondary),
                    )
                    .selectable(true),
                );
            }
            if let Some(note) = text.note {
                ui.add(
                    egui::Label::new(
                        RichText::new(note)
                            .font(theme::subheadline())
                            .color(palette.secondary),
                    )
                    .selectable(true),
                );
            }
            if let Some((label, action)) = text.button
                && super::widgets::primary_button(ui, label).clicked()
            {
                actions.push(action);
            }
        });
}

#[allow(clippy::cast_possible_truncation)] // a fraction in 0..=1
fn progress_fraction(progress: &BackupProgressFile) -> Option<f32> {
    progress
        .percent_done
        .map(|fraction| fraction.clamp(0.0, 1.0) as f32)
}

fn progress_line(progress: &BackupProgressFile) -> String {
    let mut parts = Vec::new();
    if let (Some(done), Some(total)) = (progress.files_done, progress.total_files) {
        parts.push(format!("{done} of {total} files"));
    }
    if let (Some(done), Some(total)) = (progress.bytes_done, progress.total_bytes) {
        parts.push(format!("{}/{}", bytes(done), bytes(total)));
    }
    if let Some(seconds) = progress.seconds_remaining {
        parts.push(format!("{} left", eta(seconds)));
    }
    parts.join(" \u{b7} ")
}

fn progress_block(ui: &mut Ui, progress: Option<&BackupProgressFile>, palette: &Palette) {
    progress_bar(ui, "Backup progress", progress.and_then(progress_fraction));
    let line = progress.map(progress_line).filter(|line| !line.is_empty());
    ui.label(
        RichText::new(line.unwrap_or_else(|| "Looking for changes…".to_owned()))
            .font(theme::callout())
            .color(palette.secondary),
    );
    if let Some(current) = progress.and_then(|progress| progress.current.as_deref()) {
        ui.add(
            egui::Label::new(
                RichText::new(format!("Current: {current}"))
                    .font(theme::subheadline())
                    .color(palette.dim),
            )
            .truncate(),
        );
    }
}

fn restoring_block(ui: &mut Ui, restore: &RestoreReport, palette: &Palette) {
    ui.label(
        RichText::new("Restoring\u{2026}")
            .font(theme::callout())
            .color(palette.text),
    );
    progress_bar(ui, "Restore progress", fraction(restore));
    if restore.total_files > 0 {
        ui.label(
            RichText::new(restore_line(restore))
                .font(theme::callout())
                .color(palette.secondary),
        );
    }
}

fn cell(ui: &mut Ui, width: f32, content: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, size::CONTROL_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        content,
    );
}

fn snapshots_tab(ui: &mut Ui, vm: &ViewModel, backup: &BackupSnapshot) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let id = backup.config.id.clone();
    let mut actions = Vec::new();
    match &vm.backup.snapshots {
        SnapshotsView::Loading => {
            progress_bar(ui, "Loading snapshots", None);
        }
        SnapshotsView::Failed(_) if key_problem(backup) => {
            inline_error(ui, KEY_MISSING_SENTENCE);
        }
        SnapshotsView::Failed(text) => {
            inline_error(ui, text);
            ui.add_space(space::S);
            if secondary_button(ui, "Try Again").clicked() {
                actions.push(Action::RefreshBackupSnapshots(id));
            }
        }
        SnapshotsView::Loaded(list) if list.is_empty() => {
            let button = can_back_up(backup).then_some("Back Up Now");
            if empty_state(ui, Icon::Archive, "No snapshots yet", None, button) {
                actions.push(Action::BackUpNow(id));
            }
        }
        SnapshotsView::Loaded(list) => {
            let own_host = backup_host().ok();
            for snapshot in list.iter() {
                actions.extend(snapshot_row(ui, vm.now, &id, snapshot, own_host, &palette));
            }
        }
    }
    actions
}

fn snapshot_row(
    ui: &mut Ui,
    now: Timestamp,
    backup: &str,
    snapshot: &SnapshotView,
    own_host: Option<&str>,
    palette: &Palette,
) -> Vec<Action> {
    let mut actions = Vec::new();
    list_row(ui, size::LIST_ROW_HEIGHT, |ui| {
        egui::containers::Sides::new().shrink_left().show(
            ui,
            |ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add(
                        egui::Label::new(
                            RichText::new(when(now, snapshot.time))
                                .font(theme::body())
                                .color(palette.text),
                        )
                        .truncate(),
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::S;
                        snapshot_facts(ui, snapshot, own_host, palette);
                    });
                });
            },
            |ui| {
                ui.spacing_mut().item_spacing.x = space::S;
                let whole = Action::RestoreSnapshot {
                    backup: backup.to_owned(),
                    snapshot: Box::new(snapshot.clone()),
                };
                if button_with_icon(ui, ButtonKind::Secondary, None, "Restore Snapshot\u{2026}")
                    .clicked()
                {
                    actions.push(whole);
                }
                let browse = Action::BrowseSnapshot {
                    backup: backup.to_owned(),
                    snapshot: Box::new(snapshot.clone()),
                };
                if button_with_icon(ui, ButtonKind::Secondary, None, "Browse Files\u{2026}")
                    .clicked()
                {
                    actions.push(browse);
                }
            },
        );
    });
    actions
}

fn snapshot_facts(ui: &mut Ui, snapshot: &SnapshotView, own_host: Option<&str>, palette: &Palette) {
    let text = match (snapshot.files, snapshot.bytes) {
        (Some(files), Some(total)) => format!("{} \u{b7} {}", plural_files(files), bytes(total)),
        (Some(files), None) => plural_files(files),
        (None, Some(total)) => bytes(total),
        (None, None) => String::new(),
    };
    let secondary = |text: String| {
        egui::Label::new(
            RichText::new(text)
                .font(theme::subheadline())
                .color(palette.secondary),
        )
        .truncate()
    };
    ui.add(secondary(text));
    if let Some(added) = snapshot.added_bytes {
        ui.add(secondary(format!("+{}", bytes(added))));
    }
    if own_host != Some(snapshot.host.as_str()) {
        ui.add(
            egui::Label::new(
                RichText::new(&snapshot.host)
                    .font(theme::subheadline())
                    .color(palette.dim),
            )
            .truncate(),
        );
    }
}

fn activity_tab(
    ui: &mut Ui,
    vm: &ViewModel,
    backup: &BackupSnapshot,
    palette: &Palette,
) -> Vec<Action> {
    let Some(runs) = vm.backup.runs.as_ref() else {
        return Vec::new();
    };
    if runs.is_empty() {
        empty_state(ui, Icon::History, "No activity yet", None, None);
        return Vec::new();
    }
    for run in runs.iter() {
        run_row(ui, vm, backup, run, palette);
    }
    Vec::new()
}

fn check_label(kind: Option<CheckKind>) -> &'static str {
    match kind {
        Some(CheckKind::Quick) => "Quick check",
        Some(CheckKind::Sample) => "Sample check",
        Some(CheckKind::Full) => "Full check",
        None => "Check",
    }
}

fn run_status(run: &BackupRunEntry) -> AnyStatus {
    let status = match (run.kind, run.result) {
        (RunKind::Backup, RunResult::Running) => BackupStatus::BackingUp,
        (RunKind::Check(_), RunResult::Running) => BackupStatus::Checking,
        (_, RunResult::Failed) => BackupStatus::Failing,
        (RunKind::Check(_), _) if run.check_errors.unwrap_or(0) > 0 => BackupStatus::Failing,
        _ => BackupStatus::UpToDate,
    };
    status.into()
}

fn run_counters(run: &BackupRunEntry) -> String {
    match run.kind {
        RunKind::Backup => {
            if let RunResult::DoneWithWarnings(files) = run.result {
                return format!("{} could not be read", plural_files(files));
            }
            match (run.files_changed_total(), run.data_added) {
                (Some(files), Some(added)) => format!("+{files} new \u{b7} {}", bytes(added)),
                (Some(files), None) => format!("+{files} new"),
                _ => String::new(),
            }
        }
        RunKind::Check(kind) => {
            let found = match run.check_errors {
                Some(0) => "OK".to_owned(),
                Some(1) => "1 error".to_owned(),
                Some(errors) => format!("{errors} errors"),
                None if run.result == RunResult::Failed => "Failed".to_owned(),
                None => String::new(),
            };
            format!("{} \u{b7} {found}", check_label(kind))
        }
    }
}

fn run_row(
    ui: &mut Ui,
    vm: &ViewModel,
    backup: &BackupSnapshot,
    run: &BackupRunEntry,
    palette: &Palette,
) {
    let open_id = Id::new(("backup-run-open", &backup.config.id, &run.id));
    let open = ui
        .data(|data| data.get_temp::<bool>(open_id))
        .unwrap_or(false);
    let response = list_row(ui, size::LIST_ROW_HEIGHT, |ui| {
        let chevron = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        ui.add(chevron.image(palette.dim, size::ICON));
        cell(ui, WHEN_WIDTH, |ui| {
            let text = run
                .started
                .as_ref()
                .map_or_else(|| "Unknown".to_owned(), |at| started(vm.now, at));
            ui.label(RichText::new(text).font(theme::body()).color(palette.text));
        });
        cell(ui, DURATION_WIDTH, |ui| {
            let text = if run.running() {
                "running".to_owned()
            } else {
                run.seconds.map(duration).unwrap_or_default()
            };
            ui.label(
                RichText::new(text)
                    .font(theme::body())
                    .color(palette.secondary),
            );
        });
        cell(ui, RESULT_WIDTH, |ui| {
            status_badge(ui, run_status(run), BadgeSize::Regular);
        });
        ui.add(
            egui::Label::new(
                RichText::new(run_counters(run))
                    .font(theme::body())
                    .color(palette.secondary),
            )
            .truncate(),
        );
    });
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, open, "Show log")
    });
    if response.clicked() {
        ui.data_mut(|data| data.insert_temp(open_id, !open));
    }
    if open {
        log_view(ui, run, palette);
    }
}

fn log_view(ui: &mut Ui, run: &BackupRunEntry, palette: &Palette) {
    Frame::new()
        .fill(palette.surface)
        .corner_radius(radius::CARD)
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if run.messages.is_empty() {
                ui.label(
                    RichText::new("The log of this round is empty.")
                        .font(theme::callout())
                        .color(palette.secondary),
                );
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt(("backup-run-log", &run.id))
                .max_height(LOG_MAX_HEIGHT)
                .show(ui, |ui| {
                    for message in &run.messages {
                        ui.label(
                            RichText::new(message)
                                .font(theme::monospace())
                                .color(palette.secondary),
                        );
                    }
                });
        });
    ui.add_space(space::S);
}
