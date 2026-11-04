//! The first screen: no syncs yet and the first one never confirmed.

use egui::{Align, Layout, RichText, Sense, Ui, Vec2};

use crate::icons::Icon;
use crate::theme::{self, space};

use super::Action;
use super::menu;
use super::widgets::{icon_button, primary_button, secondary_button};

/// The window title while the first screen shows.
pub const TITLE: &str = "Welcome";

/// The button that leaves the guide for the empty main window.
pub const SKIP: &str = "Set up without the guide";

/// The window menu while there is no sync: its sync rows stay, disabled.
const NO_SYNCS: menu::AppMenu = menu::AppMenu {
    has_syncs: false,
    pauses: false,
};

/// Where the user is in a step of the first sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Current,
    Done,
}

impl StepState {
    fn word(self) -> &'static str {
        match self {
            Self::Pending => "not started",
            Self::Current => "current step",
            Self::Done => "done",
        }
    }
}

const STEPS: [&str; 3] = [
    "Connect your Google account",
    "Choose a folder",
    "Review the first sync",
];

/// What decides the three steps and the button. The screen only shows while
/// there is no sync, so the first step is the only one that can be done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WelcomeView {
    /// An rclone remote exists: the account step is done.
    pub has_remote: bool,
}

impl WelcomeView {
    #[must_use]
    pub fn steps(self) -> [StepState; 3] {
        if self.has_remote {
            [StepState::Done, StepState::Current, StepState::Pending]
        } else {
            [StepState::Current, StepState::Pending, StepState::Pending]
        }
    }

    /// `Get Started` asks for an account while there is none, then for the
    /// first sync.
    fn start(self) -> Action {
        if self.has_remote {
            Action::AddSync
        } else {
            Action::AddAccount
        }
    }
}

/// The whole window while there is nothing else to show.
#[must_use]
pub fn show(ui: &mut Ui, view: &WelcomeView) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    egui::Panel::top("welcome-bar")
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(palette.window)
                .inner_margin(egui::Margin::symmetric(12, 10)),
        )
        .show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if icon_button(ui, Icon::Settings, "Settings…").clicked() {
                    actions.push(Action::OpenSettings);
                }
                actions.extend(menu::app_menu(ui, NO_SYNCS));
            });
        });
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(palette.window))
        .show(ui, |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space((ui.available_height() * 0.5 - 190.0).max(space::XXL));
                ui.add(Icon::FolderSync.image(palette.accent, 48.0));
                ui.add_space(space::S);
                ui.label(
                    RichText::new("Welcome to syncfast")
                        .font(theme::large_title())
                        .color(palette.text),
                );
                ui.add_space(space::XS);
                ui.scope(|ui| {
                    ui.set_max_width(420.0);
                    ui.label(
                        RichText::new(
                            "Keep folders on this computer in sync with Google Drive. \
                             Your files stay local first.",
                        )
                        .font(theme::body())
                        .color(palette.secondary),
                    );
                });
                ui.add_space(space::XL);
                for (index, (text, state)) in STEPS.iter().zip(view.steps()).enumerate() {
                    step(ui, index + 1, text, state);
                }
                ui.add_space(space::XL);
                if primary_button(ui, "Get Started").clicked() {
                    actions.push(view.start());
                }
                ui.add_space(space::S);
                if secondary_button(ui, SKIP).clicked() {
                    actions.push(Action::SkipWelcome);
                }
            });
        });
    actions
}

fn step(ui: &mut Ui, number: usize, text: &str, state: StepState) {
    let palette = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(Vec2::new(280.0, 30.0), Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Label,
            true,
            format!("{text}, {}", state.word()),
        )
    });
    let badge = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 11.0, rect.center().y),
        Vec2::splat(22.0),
    );
    let painter = ui.painter();
    let (ink, weight_font) = match state {
        StepState::Current => {
            painter.circle_filled(badge.center(), 11.0, palette.accent);
            (palette.on_accent, theme::headline())
        }
        StepState::Done => {
            painter.circle_stroke(badge.center(), 10.5, egui::Stroke::new(1.0, palette.accent));
            (palette.accent, theme::body())
        }
        StepState::Pending => {
            painter.circle_stroke(badge.center(), 10.5, egui::Stroke::new(1.0, palette.dim));
            (palette.dim, theme::body())
        }
    };
    if state == StepState::Done {
        Icon::Check.image(ink, 14.0).paint_at(
            ui,
            egui::Rect::from_center_size(badge.center(), Vec2::splat(14.0)),
        );
    } else {
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            number.to_string(),
            weight_font,
            ink,
        );
    }
    let (font, color) = if state == StepState::Current {
        (theme::headline(), palette.text)
    } else {
        (theme::body(), palette.secondary)
    };
    painter.text(
        egui::pos2(badge.right() + space::M, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        color,
    );
}
