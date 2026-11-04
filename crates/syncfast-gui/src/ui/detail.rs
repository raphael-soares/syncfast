//! The right side of the window: toolbar, status card and the tabs.

use egui::{Frame, Margin, RichText, Ui};
use syncfast_core::bisync_log::Progress;
use syncfast_core::state::{DisplayStatus, GateKind};

use super::format::{progress_fraction, progress_line};
use super::menu::{self, Tone};
use super::relative::synced;
use super::widgets::{
    BadgeSize, ButtonKind, button_with_icon, icon_button, inline_error, primary_button,
    progress_bar, segmented, status_badge, toolbar,
};
use super::{Action, PairRow, ViewModel, activity, conflicts, filters};
use crate::icons::Icon;
use crate::theme::{self, Palette, radius, space};

/// The side margin of everything under the toolbar, which has its own.
const SIDE: i8 = 20;

/// What the detail shows under the status card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DetailTab {
    #[default]
    Activity,
    Conflicts,
    Filters,
}

impl DetailTab {
    pub const ALL: [Self; 3] = [Self::Activity, Self::Conflicts, Self::Filters];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Activity => "Activity",
            Self::Conflicts => "Conflicts",
            Self::Filters => "Filters",
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

fn padded<R>(ui: &mut Ui, content: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .inner_margin(Margin::symmetric(SIDE, 0))
        .show(ui, content)
        .inner
}

fn pair_detail(ui: &mut Ui, vm: &ViewModel, pair: &PairRow, palette: &Palette) -> Vec<Action> {
    let mut actions = Vec::new();
    let subtitle = format!("{} ↔ {}", pair.local.display(), pair.remote);
    toolbar(ui, &pair.name, &subtitle, |ui| {
        let more = icon_button(ui, Icon::Ellipsis, "More");
        more_menu(&more, pair, palette, &mut actions);
        pause_button(ui, pair, &mut actions);
        sync_button(ui, pair, &mut actions);
    });
    padded(ui, |ui| {
        status_card(ui, pair, vm.now, palette, &mut actions);
    });
    padded(ui, |ui| {
        ui.add_space(space::L);
        let selected = DetailTab::ALL
            .iter()
            .position(|tab| *tab == vm.tab)
            .unwrap_or(0);
        let labels = DetailTab::ALL.map(DetailTab::label);
        if let Some(index) = segmented(ui, selected, &labels) {
            actions.push(Action::SelectTab(DetailTab::ALL[index]));
        }
        ui.add_space(space::S);
    });
    padded(ui, |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| match vm.tab {
                DetailTab::Activity => actions.extend(activity::show(ui, vm)),
                DetailTab::Conflicts => actions.extend(conflicts::show(ui, vm)),
                DetailTab::Filters => actions.extend(filters::show(ui, vm)),
            });
    });
    actions
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
    button: Option<(&'static str, Action)>,
}

fn card_text(pair: &PairRow, now: jiff::Timestamp) -> CardText {
    let plain = |phrase: String| CardText {
        phrase,
        button: None,
    };
    if let Some(gate) = pair.gate {
        return match gate {
            GateKind::NeedsReviewFirstRun
            | GateKind::NeedsReviewFiltersChanged
            | GateKind::NeedsReviewCriticalError
            | GateKind::TooManyDeletes
            | GateKind::AllFilesChanged => CardText {
                phrase: "Review what will change before syncing resumes.".to_owned(),
                button: Some(("Review…", Action::OpenReview(pair.id.clone()))),
            },
            GateKind::AuthError => CardText {
                phrase: format!(
                    "Reconnect \"{}\" in Settings > Accounts.",
                    pair.remote.split(':').next().unwrap_or(&pair.remote)
                ),
                button: Some(("Reconnect Account…", Action::OpenAccounts)),
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
            if pair.status == DisplayStatus::Syncing {
                progress_block(ui, pair.progress.as_ref(), palette);
                return;
            }
            let text = card_text(pair, now);
            if !text.phrase.is_empty() {
                ui.label(
                    RichText::new(text.phrase)
                        .font(theme::callout())
                        .color(palette.secondary),
                );
            }
            if let Some((label, action)) = text.button
                && primary_button(ui, label).clicked()
            {
                actions.push(action);
            }
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
