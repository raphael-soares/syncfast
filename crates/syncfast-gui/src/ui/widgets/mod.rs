//! Interface components. They read colours from the palette installed on the
//! context (`theme::palette`), never from literals.

mod alert;
mod button;
mod chip;
mod dialog;
mod empty_state;
mod folder_tree;
mod icon_button;
mod inline_error;
mod list_row;
mod modal;
mod progress_bar;
mod segmented;
mod setting_row;
mod sheet;
mod sidebar_row;
mod status_badge;
mod switch;
mod text_area;
mod text_field;
mod toolbar;
mod warning_row;

pub use alert::{AlertButton, alert};
pub use button::{
    ButtonKind, button_with_icon, destructive_button, primary_button, secondary_button,
};
pub use chip::chip;
pub use dialog::dialog;
pub use empty_state::empty_state;
pub use folder_tree::{FolderNode, FolderTree, NodeState, TreeAction, folder_tree};
pub use icon_button::icon_button;
pub use inline_error::inline_error;
pub use list_row::list_row;
pub use progress_bar::progress_bar;
pub use segmented::segmented;
pub use setting_row::setting_row;
pub use sheet::{PrimaryKind, SheetAction, SheetButtons, sheet, sheet_with_header};
pub use sidebar_row::{SidebarRow, sidebar_row};
pub use status_badge::{BadgeSize, status_badge};
pub use switch::{switch, toggle};
pub use text_area::text_area;
pub use text_field::{inline_field, secret_field, text_field};
pub use toolbar::toolbar;
pub use warning_row::warning_row;

use egui::{Color32, Pos2, Rect, Response, Ui, Vec2};

use crate::icons::Icon;
use crate::theme::{self, Palette};

fn palette_of(ui: &Ui) -> Palette {
    theme::palette(ui.ctx())
}

fn paint_icon(ui: &Ui, icon: Icon, center: Pos2, size: f32, color: Color32) {
    icon.image(color, size)
        .paint_at(ui, Rect::from_center_size(center, Vec2::splat(size)));
}

fn focus_ring(ui: &Ui, response: &Response, radius: u8) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            f32::from(radius),
            egui::Stroke::new(2.0, palette_of(ui).accent),
            egui::StrokeKind::Outside,
        );
    }
}
