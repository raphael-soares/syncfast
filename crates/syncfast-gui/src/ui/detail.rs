//! The right side of the window: toolbar, status card and the tabs.

use egui::{Frame, Id, Margin, RichText, Ui};
use syncfast_core::behavior::SyncMode;
use syncfast_core::bisync_log::Progress;
use syncfast_core::notify::attention_body;
use syncfast_core::state::{DisplayStatus, GateKind};
use syncfast_core::watch::WATCH_LIMIT_NOTICE;

use super::format::{progress_fraction, progress_line};
use super::menu::{self, Tone};
use super::relative::synced;
use super::widgets::{
    BadgeSize, ButtonKind, button_with_icon, chip, icon_button, inline_error, primary_button,
    progress_bar, segmented, status_badge, toolbar,
};
use super::{Action, PairRow, ViewModel, activity, conflicts, filters, options, recently_deleted};
use crate::icons::Icon;
use crate::theme::{self, Palette, radius, size, space};

/// The side margin of everything under the toolbar, which has its own.
const SIDE: i8 = 20;

/// What the detail shows under the status card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum DetailTab {
    #[default]
    Activity,
    RecentlyDeleted,
    Conflicts,
    Filters,
    Options,
}

impl DetailTab {
    pub const ALL: [Self; 5] = [
        Self::Activity,
        Self::RecentlyDeleted,
        Self::Conflicts,
        Self::Filters,
        Self::Options,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Activity => "Activity",
            Self::RecentlyDeleted => "Recently Deleted",
            Self::Conflicts => "Conflicts",
            Self::Filters => "Filters",
            Self::Options => "Options",
        }
    }
}

pub fn show(ui: &mut Ui, vm: &ViewModel) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    egui::CentralPanel::default()
        .frame(Frame::new().fill(palette.window))
        .show(ui, |ui| {
            if let Some(error) = &vm.error {
                padded(ui, |ui| {
                    ui.add_space(space::XL);
                    inline_error(ui, error);
                });
            }
            if let Some(backup) = vm.selected_backup() {
                actions.extend(super::backup_detail::detail(ui, vm, backup));
                return;
            }
            match vm.selected_pair() {
                Some(pair) => actions.extend(pair_detail(ui, vm, pair, &palette)),
                None => padded(ui, |ui| {
                    ui.add_space(space::XL);
                    if super::widgets::empty_state(
                        ui,
                        Icon::FolderSync,
                        "No syncs yet",
                        Some("Keep a folder on this computer and a folder in the cloud the same."),
                        Some("Add Sync…"),
                    ) {
                        actions.push(Action::AddSync);
                    }
                }),
            }
        });
    actions
}

pub(super) fn padded<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .inner_margin(Margin::symmetric(SIDE, 0))
        .show(ui, content)
        .inner
}

/// True on the first frame `tab` is drawn for `pair_id`, and again when
/// frames stopped for a while (another tab, another sync): what the tab
/// lists is read again when the user comes back to it.
pub(super) fn just_opened(ctx: &egui::Context, tab: DetailTab, pair_id: &str) -> bool {
    let frame = ctx.cumulative_frame_nr();
    let key = Id::new(("detail-tab-frame", tab, pair_id));
    let previous = ctx.data_mut(|data| {
        let previous = data.get_temp::<u64>(key);
        data.insert_temp(key, frame);
        previous
    });
    previous.is_none_or(|previous| previous.saturating_add(1) < frame)
}

fn pair_detail(ui: &mut Ui, vm: &ViewModel, pair: &PairRow, palette: &Palette) -> Vec<Action> {
    let mut actions = Vec::new();
    let sync_mode = pair.effective.behavior.mode;
    let arrow = match sync_mode {
        SyncMode::Bisync => "\u{2194}",
        SyncMode::Upload | SyncMode::MirrorUp => "\u{2192}",
        SyncMode::Download | SyncMode::MirrorDown => "\u{2190}",
    };
    let subtitle = format!("{} {arrow} {}", pair.local.display(), pair.remote);
    toolbar(ui, &pair.name, &subtitle, |ui| {
        let more = icon_button(ui, Icon::Ellipsis, "More");
        more_menu(&more, pair, palette, &mut actions);
        pause_button(ui, pair, &mut actions);
        sync_button(ui, pair, &mut actions);
        if sync_mode != SyncMode::Bisync {
            chip(ui, options::mode_label(sync_mode.name()));
        }
    });
    padded(ui, |ui| {
        status_card(ui, pair, vm.now, palette, &mut actions);
    });
    let tabs = tabs_of(sync_mode);
    let tab = if tabs.contains(&vm.tab) {
        vm.tab
    } else {
        tabs.first().copied().unwrap_or_default()
    };
    padded(ui, |ui| {
        ui.add_space(space::L);
        let selected = tabs.iter().position(|shown| *shown == tab).unwrap_or(0);
        let labels: Vec<&str> = tabs.iter().map(|shown| shown.label()).collect();
        if let Some(index) = segmented(ui, selected, &labels) {
            actions.push(Action::SelectTab(tabs[index]));
        }
        ui.add_space(space::S);
    });
    padded(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match tab {
                DetailTab::Activity => actions.extend(activity::show(ui, vm)),
                DetailTab::RecentlyDeleted => actions.extend(recently_deleted::show(ui, vm)),
                DetailTab::Conflicts => actions.extend(conflicts::show(ui, vm)),
                DetailTab::Filters => actions.extend(filters::show(ui, vm)),
                DetailTab::Options => actions.extend(options::sync(ui, vm)),
            });
    });
    actions
}

/// Conflicts only exist in a two-way sync, and a mirror of Drive keeps
/// nothing it deletes.
fn tabs_of(mode: SyncMode) -> Vec<DetailTab> {
    DetailTab::ALL
        .into_iter()
        .filter(|tab| match tab {
            DetailTab::Conflicts => mode == SyncMode::Bisync,
            DetailTab::RecentlyDeleted => mode != SyncMode::MirrorDown,
            _ => true,
        })
        .collect()
}

fn sync_button(ui: &mut Ui, pair: &PairRow, actions: &mut Vec<Action>) {
    let clicked = ui
        .add_enabled_ui(pair.can_sync(), |ui| {
            button_with_icon(ui, ButtonKind::Primary, Some(Icon::Refresh), "Sync Now").clicked()
        })
        .inner;
    if clicked {
        actions.push(Action::SyncNow(pair.id.clone()));
    }
}

fn pause_button(ui: &mut Ui, pair: &PairRow, actions: &mut Vec<Action>) {
    let (icon, label) = if pair.enabled {
        (Icon::Pause, "Pause")
    } else {
        (Icon::Play, "Resume")
    };
    if button_with_icon(ui, ButtonKind::Secondary, Some(icon), label).clicked() {
        actions.push(Action::SetEnabled(pair.id.clone(), !pair.enabled));
    }
}

fn more_menu(
    button: &egui::Response,
    pair: &PairRow,
    palette: &Palette,
    actions: &mut Vec<Action>,
) {
    menu::popup(button, palette, |ui| {
        if menu::item(ui, palette, Icon::Folder, "Open Local Folder", Tone::Normal) {
            actions.push(Action::OpenLocalFolder(pair.id.clone()));
        }
        if menu::item(
            ui,
            palette,
            Icon::History,
            "Open Versions Folder",
            Tone::Normal,
        ) {
            actions.push(Action::OpenVersionsFolder(pair.id.clone()));
        }
        ui.separator();
        if menu::item(ui, palette, Icon::Trash, "Remove Sync…", Tone::Danger) {
            actions.push(Action::RemoveSync(pair.id.clone()));
        }
    });
}

/// What the card says beside the status, and the button that goes with it.
struct CardText {
    phrase: String,
    /// Raw text of the failure, selectable so the user can copy it.
    detail: Option<String>,
    button: Option<(&'static str, Action)>,
}

fn card_text(pair: &PairRow, now: jiff::Timestamp) -> CardText {
    let plain = |phrase: String| CardText {
        phrase,
        detail: None,
        button: None,
    };
    if let Some(gate) = pair.gate {
        return match gate {
            GateKind::NeedsReviewFirstRun
            | GateKind::NeedsReviewFiltersChanged
            | GateKind::NeedsReviewCriticalError
            | GateKind::NeedsReviewModeChanged
            | GateKind::TooManyDeletes
            | GateKind::AllFilesChanged => {
                let review = "Review what will change before syncing resumes.";
                let stopped = gate == GateKind::TooManyDeletes && !pair.gate_detail.is_empty();
                CardText {
                    phrase: if stopped {
                        format!("{} {review}", pair.gate_detail)
                    } else {
                        review.to_owned()
                    },
                    detail: None,
                    button: Some(("Review…", Action::OpenReview(pair.id.clone()))),
                }
            }
            GateKind::AuthError => CardText {
                phrase: format!(
                    "Reconnect \"{}\" in Settings > Accounts.",
                    pair.remote.split(':').next().unwrap_or(&pair.remote)
                ),
                detail: None,
                button: Some(("Reconnect Account…", Action::OpenAccounts)),
            },
            GateKind::NeedsAttention => CardText {
                phrase: attention_body(pair.gate_fault, &pair.gate_detail).to_owned(),
                detail: Some(pair.gate_detail.clone()).filter(|detail| !detail.is_empty()),
                button: Some(("Try Again", Action::SyncNow(pair.id.clone()))),
            },
            GateKind::LocalMissing => plain(format!("{} is missing.", pair.local.display())),
            GateKind::ConfigError => plain(if pair.gate_detail.is_empty() {
                "The configuration of this sync has a problem.".to_owned()
            } else {
                pair.gate_detail.clone()
            }),
        };
    }
    plain(match pair.status {
        DisplayStatus::Conflicts(_) => "Both versions were kept. Choose which one to keep.".into(),
        DisplayStatus::Paused => "This sync is paused. Resume it to sync again.".into(),
        DisplayStatus::Offline => "No network. Syncing resumes when it is back.".into(),
        DisplayStatus::Failing => format!(
            "The last {} syncs failed in a row.",
            pair.consecutive_failures
        ),
        DisplayStatus::Retrying => "The last sync failed. It will try again soon.".into(),
        DisplayStatus::UpToDate => synced(now, pair.last_success),
        DisplayStatus::Syncing | DisplayStatus::Error | DisplayStatus::NeedsReview => String::new(),
    })
}

fn status_card(
    ui: &mut Ui,
    pair: &PairRow,
    now: jiff::Timestamp,
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
            status_badge(ui, pair.status, BadgeSize::Large);
            safety_notice(ui, pair, palette);
            if pair.status == DisplayStatus::Syncing {
                progress_block(ui, pair.progress.as_ref(), palette);
                return;
            }
            let text = card_text(pair, now);
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
            if pair.realtime_off {
                ui.label(
                    RichText::new(WATCH_LIMIT_NOTICE)
                        .font(theme::callout())
                        .color(palette.warning),
                );
            }
            if let Some((label, action)) = text.button
                && primary_button(ui, label).clicked()
            {
                actions.push(action);
            }
        });
}

/// Shown when a setting of the sync makes it less safe than the built-in
/// values; the tooltip names the settings.
fn safety_notice(ui: &mut Ui, pair: &PairRow, palette: &Palette) {
    let lowered = pair.lowered_safety();
    if lowered.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS;
        ui.add(Icon::CircleAlert.image(palette.warning, size::ICON));
        ui.add(
            egui::Label::new(
                RichText::new("Safety lowered")
                    .font(theme::callout())
                    .color(palette.warning),
            )
            .sense(egui::Sense::hover()),
        )
        .on_hover_text(format!(
            "Less safe than the built-in values: {}",
            lowered.join(", ")
        ));
    });
}

fn progress_block(ui: &mut Ui, progress: Option<&Progress>, palette: &Palette) {
    let fraction = progress.and_then(progress_fraction);
    progress_bar(ui, "Sync progress", fraction);
    let line = match progress {
        Some(progress) if progress.total_transfers > 0 => progress_line(progress),
        _ => "Checking for changes…".to_owned(),
    };
    ui.label(
        RichText::new(line)
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
