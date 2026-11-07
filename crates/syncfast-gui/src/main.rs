use std::process::ExitCode;

use anyhow::Context as _;
use fastframe_instance::Claim;
use fastframe_shell::{Lease, Shell, Waker};
use syncfast_core::paths::Paths;
use syncfast_gui::app::{App, app_id};
use syncfast_gui::app_settings;
use syncfast_gui::backend;
use syncfast_gui::gui_state::{self, GuiState};
use syncfast_gui::instance::{self, Inbox};
use syncfast_gui::theme::{Themes, size};

const WINDOW_ICON: &[u8] = include_bytes!("../assets/app-icon/syncfast-256.png");
const LOG_FILTER: &str = "warn,syncfast_gui=info,syncfast_core=info";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    Open,
    StartHidden,
    ReloadThemes,
}

/// `syncfast-gui [--start-hidden | reload-themes]`.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Launch, String> {
    let mut args = args.into_iter();
    let launch = match args.next().as_deref() {
        None => Launch::Open,
        Some("--start-hidden") => Launch::StartHidden,
        Some("reload-themes") => Launch::ReloadThemes,
        Some(other) => return Err(format!("unknown argument \"{other}\"")),
    };
    match args.next() {
        None => Ok(launch),
        Some(extra) => Err(format!("unexpected argument \"{extra}\"")),
    }
}

fn main() -> ExitCode {
    // Without logging nothing can be reported, but the app still runs.
    let _ = fastframe_log::Logging::new("syncfast-gui", env!("CARGO_PKG_VERSION"))
        .filter(LOG_FILTER)
        .init();
    let launch = match parse_args(std::env::args().skip(1)) {
        Ok(launch) => launch,
        Err(message) => {
            log::error!("{message}; usage: syncfast-gui [--start-hidden | reload-themes]");
            return ExitCode::from(2);
        }
    };
    match launch {
        Launch::ReloadThemes => reload_themes(),
        Launch::Open | Launch::StartHidden => match run(launch == Launch::StartHidden) {
            Ok(code) => code,
            Err(err) => {
                log::error!("{err:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn reload_themes() -> ExitCode {
    match instance::ask_to_reload_themes() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            log::error!("not running");
            ExitCode::FAILURE
        }
        Err(err) => {
            log::error!("cannot reach the running syncfast: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(start_hidden: bool) -> anyhow::Result<ExitCode> {
    let paths = Paths::from_env().context("cannot find the user's folders")?;
    let waker = Waker::default();
    let inbox = Inbox::default();
    let _claim = match instance::claim(&inbox, &waker) {
        Claim::First(guard) => guard,
        Claim::Running(_) => return Ok(ExitCode::SUCCESS),
        Claim::Declined | Claim::Unanswered => {
            log::error!("syncfast is already running but did not answer");
            return Ok(ExitCode::FAILURE);
        }
    };
    let gui = GuiState::load(&gui_state::file(&paths));
    let theme = app_settings::migrate_theme(&paths);
    // Read before the frame loop starts; reloads go through the backend.
    let themes = Themes::load(&backend::themes_dir(&paths));
    let app = App::new(&paths, &waker, inbox, gui, themes, true)
        .context("cannot start the backend")?
        .with_theme(theme);
    Shell::new(app, &waker)
        .start_hidden(start_hidden)
        .idle(fastframe_tray::idle)
        .run(|lease: Lease<App>| {
            let geometry = lease.peek(App::geometry);
            eframe::run_native(
                "Syncs",
                native_options(geometry),
                Box::new(move |cc| {
                    let mut app = lease.take(&cc.egui_ctx);
                    app.attach(&cc.egui_ctx);
                    Ok(Box::new(Window {
                        app,
                        recovery_checked: false,
                    }))
                }),
            )
        })
        .map_err(|err| anyhow::anyhow!("the window failed: {err}"))?;
    Ok(ExitCode::SUCCESS)
}

fn native_options(geometry: fastframe_shell::window::Geometry) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Syncs")
        .with_app_id(app_id())
        .with_inner_size(size::WINDOW_DEFAULT);
    match eframe::icon_data::from_png_bytes(WINDOW_ICON) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(err) => log::warn!("cannot load the window icon: {err}"),
    }
    let viewport = geometry
        .apply(viewport)
        .with_min_inner_size(size::WINDOW_MIN);
    eframe::NativeOptions {
        viewport,
        // The geometry lives in gui.json; eframe's own copy would fight it.
        persist_window: false,
        ..Default::default()
    }
}

/// eframe's view of one window; the app goes back to the shell when it drops.
struct Window {
    app: fastframe_shell::Held<App>,
    recovery_checked: bool,
}

impl eframe::App for Window {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.app.window_colour().to_normalized_gamma_f32()
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !std::mem::replace(&mut self.recovery_checked, true) {
            fastframe_shell::window::recover_offscreen(ctx, frame);
        }
        self.app.window_logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.frame_ui(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Launch, String> {
        parse_args(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn the_embedded_window_icon_is_a_square_png() {
        let icon = eframe::icon_data::from_png_bytes(WINDOW_ICON).unwrap();

        assert_eq!((icon.width, icon.height), (256, 256));
    }

    #[test]
    fn no_argument_opens_the_window() {
        assert_eq!(parse(&[]), Ok(Launch::Open));
    }

    #[test]
    fn start_hidden_and_reload_themes_are_recognised() {
        assert_eq!(parse(&["--start-hidden"]), Ok(Launch::StartHidden));
        assert_eq!(parse(&["reload-themes"]), Ok(Launch::ReloadThemes));
    }

    #[test]
    fn unknown_and_extra_arguments_are_refused() {
        assert_eq!(
            parse(&["--bogus"]),
            Err("unknown argument \"--bogus\"".to_owned())
        );
        assert_eq!(
            parse(&["reload-themes", "now"]),
            Err("unexpected argument \"now\"".to_owned())
        );
    }
}
