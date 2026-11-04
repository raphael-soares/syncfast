//! Popup menus: the rows they share and the window's own menu.

use egui::{Frame, Margin, Sense, Ui, Vec2};

use super::Action;
use super::widgets::icon_button;
use crate::icons::Icon;
use crate::theme::{self, Palette, radius};

const MENU_WIDTH: f32 = 220.0;
const MENU_ROW_HEIGHT: f32 = 28.0;
const MENU_ICON_GAP: f32 = 8.0;

/// The tooltip and accessible name of the window menu's button.
pub const APP_MENU: &str = "syncfast Menu";

/// How a row looks and whether it answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Danger,
    /// Shown but unavailable: a menu keeps its rows instead of hiding them.
    Disabled,
}

/// The popup that opens from `button`, with `rows` inside.
pub fn popup(button: &egui::Response, palette: &Palette, rows: impl FnOnce(&mut Ui)) {
    let frame = Frame::new()
        .fill(palette.overlay)
        .stroke(egui::Stroke::new(1.0, palette.outline))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::same(6));
    egui::Popup::menu(button)
        .width(MENU_WIDTH)
        .frame(frame)
        .show(rows);
}

/// One row; true when it was clicked.
pub fn item(ui: &mut Ui, palette: &Palette, icon: Icon, label: &str, tone: Tone) -> bool {
    let enabled = tone != Tone::Disabled;
    let color = match tone {
        Tone::Normal => palette.text,
        Tone::Danger => palette.danger,
        Tone::Disabled => palette.dim,
    };
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), MENU_ROW_HEIGHT), sense);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if enabled && response.hovered() {
        ui.painter()
            .rect_filled(rect, f32::from(radius::CONTROL), palette.surface_hover);
    }
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(
            rect.left() + MENU_ICON_GAP + theme::size::ICON / 2.0,
            rect.center().y,
        ),
        Vec2::splat(theme::size::ICON),
    );
    icon.image(color, theme::size::ICON).paint_at(ui, icon_rect);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), theme::body(), color);
    let pos = egui::pos2(
        icon_rect.right() + MENU_ICON_GAP,
        rect.center().y - galley.size().y / 2.0,
    );
    ui.painter().galley(pos, galley, color);
    enabled && response.clicked()
}

/// What the window menu offers, the same rows as the tray's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppMenu {
    /// There is at least one sync: `Sync All Now` and `Pause All` answer.
    pub has_syncs: bool,
    /// Some sync is active, so the pause row pauses; otherwise it resumes.
    pub pauses: bool,
}

impl AppMenu {
    fn sync_tone(self) -> Tone {
        if self.has_syncs {
            Tone::Normal
        } else {
            Tone::Disabled
        }
    }
}

/// The button that opens the window menu, laid out where the caller puts it.
#[must_use]
pub fn app_menu(ui: &mut Ui, menu: AppMenu) -> Vec<Action> {
    let palette = theme::palette(ui.ctx());
    let mut actions = Vec::new();
    let button = icon_button(ui, Icon::Ellipsis, APP_MENU);
    popup(&button, &palette, |ui| {
        if item(
            ui,
            &palette,
            Icon::Refresh,
            "Sync All Now",
            menu.sync_tone(),
        ) {
            actions.push(Action::SyncAll);
        }
        let (icon, label) = if menu.pauses {
            (Icon::Pause, "Pause All")
        } else {
            (Icon::Play, "Resume All")
        };
        if item(ui, &palette, icon, label, menu.sync_tone()) {
            actions.push(Action::SetAllEnabled(!menu.pauses));
        }
        ui.separator();
        if item(ui, &palette, Icon::EyeOff, "Hide Window", Tone::Normal) {
            actions.push(Action::HideWindow);
        }
        if item(ui, &palette, Icon::LogOut, "Quit syncfast", Tone::Normal) {
            actions.push(Action::Quit);
        }
    });
    actions
}
