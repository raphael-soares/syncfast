//! A harness for the screens: a view model in, the actions out.

use egui::Ui;
use egui_kittest::Harness;
use jiff::Timestamp;
use syncfast_core::state::DisplayStatus;
use syncfast_gui::theme::{self, Palette};
use syncfast_gui::ui::{Action, PairRow, ViewModel};

pub struct Frame {
    pub vm: ViewModel,
    pub actions: Vec<Action>,
}

pub fn now() -> Timestamp {
    "2026-11-03T12:00:00Z".parse().unwrap()
}

pub fn pair(id: &str, name: &str) -> PairRow {
    PairRow {
        id: id.into(),
        name: name.into(),
        local: format!("/home/ada/{id}").into(),
        remote: format!("gdrive:{id}"),
        enabled: true,
        status: DisplayStatus::UpToDate,
        gate: None,
        last_success: None,
        progress: None,
        gate_detail: String::new(),
        consecutive_failures: 0,
        realtime_off: false,
        presets: vec!["system_junk".into(), "dev_dirs".into()],
        filters: Vec::new(),
    }
}

pub fn view_model(pairs: Vec<PairRow>, selected: Option<&str>) -> ViewModel {
    ViewModel {
        pairs,
        selected: selected.map(ToOwned::to_owned),
        sidebar_width: 240.0,
        now: now(),
        ..ViewModel::default()
    }
}

/// Installs the theme and tells whether fonts are usable: `set_fonts` takes
/// effect on the next pass, so the first one must not draw.
fn ready(ui: &Ui) -> bool {
    theme::install_with(
        ui.ctx(),
        &Palette::dark(),
        fastframe_text::TextRendering::platform_default(),
    );
    ui.ctx().fonts(|fonts| {
        fonts
            .families()
            .contains(&egui::FontFamily::Name("inter-semibold".into()))
    })
}

/// Draws `screen` with the view model every frame and collects its actions.
pub fn harness(
    vm: ViewModel,
    mut screen: impl FnMut(&mut Ui, &ViewModel) -> Vec<Action> + 'static,
) -> Harness<'static, Frame> {
    let mut harness = Harness::builder()
        .with_size(egui::vec2(900.0, 600.0))
        .build_ui_state(
            move |ui, frame: &mut Frame| {
                if ready(ui) {
                    let actions = screen(ui, &frame.vm);
                    frame.actions.extend(actions);
                }
            },
            Frame {
                vm,
                actions: Vec::new(),
            },
        );
    harness.run_ok();
    harness
}
