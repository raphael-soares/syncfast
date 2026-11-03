use egui::{Sense, Ui, Vec2};
use syncfast_core::state::DisplayStatus;

use crate::icons::{Icon, status_icon};
use crate::theme::{self, radius, size, space};

use super::{focus_ring, paint_icon, palette_of};

#[derive(Debug)]
pub struct SidebarRow<'a> {
    pub icon: Icon,
    pub title: &'a str,
    /// Metadata line such as `Synced 3 min ago`. With it the row is 44 high,
    /// otherwise 32.
    pub subtitle: Option<&'a str>,
    pub selected: bool,
    /// Shown as a compact badge at the trailing edge; its text is the tooltip.
    pub status: Option<DisplayStatus>,
}

/// A selectable sidebar entry. The title is its accessible name.
pub fn sidebar_row(ui: &mut Ui, row: &SidebarRow<'_>) -> egui::Response {
    let palette = palette_of(ui);
    let height = if row.subtitle.is_some() {
        size::LIST_ROW_HEIGHT
    } else {
        size::SIDEBAR_ROW_HEIGHT
    };
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    let label = accessible_label(row);
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            row.selected,
            &label,
        )
    });
    if ui.is_rect_visible(rect) {
        let fill = if row.selected {
            Some(palette.surface_active)
        } else if response.hovered() {
            Some(palette.surface_hover)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter()
                .rect_filled(rect, f32::from(radius::CARD), fill);
        }
        let inner = rect.shrink2(Vec2::new(space::S, 0.0));
        paint_icon(
            ui,
            row.icon,
            egui::pos2(inner.left() + size::ICON / 2.0, rect.center().y),
            size::ICON,
            palette.secondary,
        );
        let text_x = inner.left() + size::ICON + 10.0;
        let trailing = if row.status.is_some() {
            size::ICON + space::S
        } else {
            0.0
        };
        let text_width = (inner.right() - trailing - text_x).max(0.0);
        let title = truncated(
            ui.painter(),
            row.title,
            theme::headline(),
            palette.text,
            text_width,
        );
        let title_height = title.size().y;
        match row.subtitle {
            Some(subtitle) => {
                let sub = truncated(
                    ui.painter(),
                    subtitle,
                    theme::subheadline(),
                    palette.secondary,
                    text_width,
                );
                let total = title_height + sub.size().y + 2.0;
                let top = rect.center().y - total / 2.0;
                ui.painter()
                    .galley(egui::pos2(text_x, top), title, palette.text);
                ui.painter().galley(
                    egui::pos2(text_x, top + title_height + 2.0),
                    sub,
                    palette.secondary,
                );
            }
            None => {
                ui.painter().galley(
                    egui::pos2(text_x, rect.center().y - title_height / 2.0),
                    title,
                    palette.text,
                );
            }
        }
        if let Some(status) = row.status {
            let color = theme::tone(&palette, status.tone());
            paint_icon(
                ui,
                status_icon(status),
                egui::pos2(inner.right() - size::ICON / 2.0, rect.center().y),
                size::ICON,
                color,
            );
        }
    }
    focus_ring(ui, &response, radius::CARD);
    match row.status {
        Some(status) => response.on_hover_text(status.text()),
        None => response,
    }
}

/// `<title>, <status>, <subtitle>`: the text painted around the title would
/// otherwise be invisible to screen readers.
fn accessible_label(row: &SidebarRow<'_>) -> String {
    let status = row.status.map(DisplayStatus::text);
    [
        Some(row.title.to_owned()),
        status,
        row.subtitle.map(str::to_owned),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ")
}

/// One line of text that ends in an ellipsis when it is wider than `width`.
fn truncated(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    painter.layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether the text was cut, and how wide the line came out.
    fn laid_out(text: &str, width: f32) -> (bool, f32) {
        let ctx = egui::Context::default();
        let mut result = (false, 0.0);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let galley = truncated(
                ui.painter(),
                text,
                egui::FontId::proportional(13.0),
                egui::Color32::WHITE,
                width,
            );
            result = (galley.elided, galley.size().x);
        });
        output.textures_delta.clear();
        result
    }

    #[test]
    fn a_title_wider_than_the_row_is_cut_with_an_ellipsis_inside_the_width() {
        let (elided, width) = laid_out("A very long sync name that cannot possibly fit", 80.0);

        assert!(elided);
        assert!(width <= 80.5, "{width}");
    }

    #[test]
    fn a_title_that_fits_is_left_alone() {
        let (elided, _) = laid_out("Notes", 200.0);

        assert!(!elided);
    }
}
