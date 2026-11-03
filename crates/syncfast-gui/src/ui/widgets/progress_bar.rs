use std::time::Duration;

use egui::{Sense, Ui, Vec2};

use crate::theme::radius;

use super::palette_of;

const HEIGHT: f32 = 6.0;
const SWEEP_SECONDS: f64 = 1.4;
const SWEEP_WIDTH: f32 = 0.3;

/// A progress bar across the available width. `Some(fraction)` in `0..=1` is
/// determinate and exposes its percentage to accessibility tools; `None`
/// sweeps a segment back and forth for work of unknown size.
pub fn progress_bar(ui: &mut Ui, label: &str, fraction: Option<f32>) -> egui::Response {
    let palette = palette_of(ui);
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), Sense::hover());
    let percent = fraction.map(|fraction| f64::from(fraction.clamp(0.0, 1.0)) * 100.0);
    response.widget_info(|| {
        let mut info =
            egui::WidgetInfo::labeled(egui::WidgetType::ProgressIndicator, ui.is_enabled(), label);
        info.value = percent;
        info
    });
    if ui.is_rect_visible(rect) {
        let round = f32::from(radius::CONTROL) / 2.0;
        ui.painter()
            .rect_filled(rect, round, palette.surface_active);
        let fill = if let Some(fraction) = fraction {
            let width = rect.width() * fraction.clamp(0.0, 1.0);
            egui::Rect::from_min_size(rect.min, Vec2::new(width, rect.height()))
        } else {
            let width = rect.width() * SWEEP_WIDTH;
            let left = rect.left() + (rect.width() - width) * sweep_position(ui);
            ui.ctx().request_repaint_after(Duration::from_millis(16));
            egui::Rect::from_min_size(
                egui::pos2(left, rect.top()),
                Vec2::new(width, rect.height()),
            )
        };
        if fill.width() > 0.0 {
            ui.painter().rect_filled(fill, round, palette.accent);
        }
    }
    response
}

/// Position of the sweeping segment, `0.0..=1.0`, bouncing at the ends.
#[allow(clippy::cast_possible_truncation)] // the value is within 0..=2
fn sweep_position(ui: &Ui) -> f32 {
    let time = ui.input(|input| input.time);
    let phase = (time % (SWEEP_SECONDS * 2.0)) / SWEEP_SECONDS;
    (if phase > 1.0 { 2.0 - phase } else { phase }) as f32
}
