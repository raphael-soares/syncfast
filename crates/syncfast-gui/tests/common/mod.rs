//! A harness for the screens: a view model in, the actions out.

use egui::Ui;
use egui_kittest::Harness;
use jiff::Timestamp;
use syncfast_core::config::Config;
use syncfast_core::settings::Effective;
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
        gate_fault: None,
        consecutive_failures: 0,
        realtime_off: false,
        presets: vec!["system_junk".into(), "dev_dirs".into()],
        filters: Vec::new(),
        effective: Effective::default(),
    }
}

/// The view model a `config.toml` gives: every sync with what it runs with,
/// and the `[defaults]`.
#[allow(dead_code)]
pub fn from_config(text: &str, selected: Option<&str>) -> ViewModel {
    let config = Config::parse(text, std::path::Path::new("/home/ada")).unwrap();
    let pairs = config
        .pairs()
        .iter()
        .map(|pair| PairRow {
            effective: config.effective(pair),
            ..self::pair(&pair.id, &pair.name)
        })
        .collect();
    ViewModel {
        defaults: config.defaults(),
        defaults_overrides: config.defaults_overrides().clone(),
        ..view_model(pairs, selected)
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
fn ready(ui: &Ui, palette: &Palette) -> bool {
    theme::install_with(
        ui.ctx(),
        palette,
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
    screen: impl FnMut(&mut Ui, &ViewModel) -> Vec<Action> + 'static,
) -> Harness<'static, Frame> {
    harness_in(vm, egui::vec2(900.0, 600.0), Palette::dark(), screen)
}

/// Like [`harness`], in a window of `size` painted with `palette`.
pub fn harness_in(
    vm: ViewModel,
    size: egui::Vec2,
    palette: Palette,
    mut screen: impl FnMut(&mut Ui, &ViewModel) -> Vec<Action> + 'static,
) -> Harness<'static, Frame> {
    let mut harness = Harness::builder().with_size(size).build_ui_state(
        move |ui, frame: &mut Frame| {
            if ready(ui, &palette) {
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
