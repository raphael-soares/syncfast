//! Palette, theme catalogue, typography scale, measures and the egui style.
//!
//! Every colour in the interface comes from a [`Palette`]; widgets read the
//! active one with [`palette`].

use std::path::{Path, PathBuf};

use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle, Vec2};
use fastframe_fonts::{FontSetup, Weight};
use fastframe_theme::{Base, CustomTheme};
use syncfast_core::state::Tone;

use crate::icons::Icon;

/// The sixteen shared interface colours. Palette files name them as in
/// `fastframe_theme::BASE_COLORS`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub window: Color32,
    pub panel: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    pub outline: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub overlay: Color32,
    pub shadow: Color32,
}

impl Palette {
    /// The tinted background of an in-line warning.
    #[must_use]
    pub fn warning_fill(&self) -> Color32 {
        self.warning.gamma_multiply(0.08)
    }

    /// The border of an in-line warning.
    #[must_use]
    pub fn warning_border(&self) -> Color32 {
        self.warning.gamma_multiply(0.25)
    }

    #[must_use]
    pub const fn dark() -> Self {
        Self {
            dark: true,
            window: Color32::from_rgb(0x0f, 0x11, 0x14),
            panel: Color32::from_rgb(0x15, 0x18, 0x1c),
            surface: Color32::from_rgb(0x1d, 0x21, 0x27),
            surface_hover: Color32::from_rgb(0x26, 0x2b, 0x33),
            surface_active: Color32::from_rgb(0x2f, 0x35, 0x3f),
            outline: Color32::from_rgb(0x2a, 0x30, 0x38),
            text: Color32::from_rgb(0xf2, 0xf4, 0xf6),
            secondary: Color32::from_rgb(0xa9, 0xb1, 0xbc),
            dim: Color32::from_rgb(0x6e, 0x77, 0x84),
            accent: Color32::from_rgb(0xa7, 0x8b, 0xfa),
            accent_hover: Color32::from_rgb(0xc4, 0xb5, 0xfd),
            on_accent: Color32::from_rgb(0x1e, 0x10, 0x33),
            danger: Color32::from_rgb(0xf5, 0x71, 0x7f),
            warning: Color32::from_rgb(0xf2, 0xb8, 0x5c),
            overlay: Color32::from_rgb(0x22, 0x27, 0x2e),
            shadow: Color32::from_black_alpha(0x8c),
        }
    }

    #[must_use]
    pub const fn light() -> Self {
        Self {
            dark: false,
            window: Color32::from_rgb(0xf8, 0xf9, 0xfb),
            panel: Color32::from_rgb(0xff, 0xff, 0xff),
            surface: Color32::from_rgb(0xee, 0xf0, 0xf3),
            surface_hover: Color32::from_rgb(0xe3, 0xe6, 0xeb),
            surface_active: Color32::from_rgb(0xd7, 0xdb, 0xe1),
            outline: Color32::from_rgb(0xdd, 0xe1, 0xe6),
            text: Color32::from_rgb(0x14, 0x17, 0x1a),
            secondary: Color32::from_rgb(0x53, 0x5b, 0x66),
            dim: Color32::from_rgb(0x8b, 0x93, 0x9e),
            accent: Color32::from_rgb(0x7c, 0x3a, 0xed),
            accent_hover: Color32::from_rgb(0x6d, 0x28, 0xd9),
            on_accent: Color32::WHITE,
            danger: Color32::from_rgb(0xd6, 0x3b, 0x4c),
            warning: Color32::from_rgb(0xb8, 0x7a, 0x14),
            overlay: Color32::WHITE,
            shadow: Color32::from_black_alpha(0x32),
        }
    }
}

impl fastframe_theme::Palette for Palette {
    fn base(base: Base) -> Self {
        match base {
            Base::Dark => Self::dark(),
            Base::Light => Self::light(),
        }
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        match name {
            "window" => self.window = color,
            "panel" => self.panel = color,
            "surface" => self.surface = color,
            "surface_hover" => self.surface_hover = color,
            "surface_active" => self.surface_active = color,
            "outline" => self.outline = color,
            "text" => self.text = color,
            "secondary" => self.secondary = color,
            "dim" => self.dim = color,
            "accent" => self.accent = color,
            "accent_hover" => self.accent_hover = color,
            "on_accent" => self.on_accent = color,
            "danger" => self.danger = color,
            "warning" => self.warning = color,
            "overlay" => self.overlay = color,
            "shadow" => self.shadow = color,
            _ => return false,
        }
        true
    }

    /// Menus and sheets follow a file's window when it leaves `overlay` out:
    /// a step toward the text in dark palettes, the panel in light ones.
    fn derive(&mut self, given: &std::collections::BTreeSet<&str>) {
        if given.contains("overlay") || !given.contains("window") {
            return;
        }
        self.overlay = if self.dark {
            self.window.lerp_to_gamma(self.text, 0.08)
        } else {
            self.panel
        };
    }
}

impl fastframe_theme::PaletteBase for Palette {
    fn palette_base(&self) -> Base {
        Base::from_dark(self.dark)
    }
}

/// The colour that paints a status tone.
#[must_use]
pub fn tone(palette: &Palette, tone: Tone) -> Color32 {
    match tone {
        Tone::Accent => palette.accent,
        Tone::Danger => palette.danger,
        Tone::Warning => palette.warning,
        Tone::Dim => palette.dim,
        Tone::Secondary => palette.secondary,
    }
}

/// Which palette the user picked. Serialized as `"system"`, `"light"`,
/// `"dark"` or the theme file's name.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
    Named(String),
}

impl From<String> for ThemeChoice {
    fn from(value: String) -> Self {
        match value.as_str() {
            "system" => Self::System,
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::Named(value),
        }
    }
}

impl From<ThemeChoice> for String {
    fn from(choice: ThemeChoice) -> Self {
        match choice {
            ThemeChoice::System => "system".to_owned(),
            ThemeChoice::Light => "light".to_owned(),
            ThemeChoice::Dark => "dark".to_owned(),
            ThemeChoice::Named(name) => name,
        }
    }
}

/// The built-in palettes plus the user's palette files.
#[derive(Clone, Debug)]
pub struct Themes {
    dir: PathBuf,
    custom: Vec<CustomTheme<Palette>>,
    errors: Vec<String>,
}

impl Themes {
    /// Reads every `*.json` palette in `themes_dir`. A missing folder is not
    /// an error.
    #[must_use]
    pub fn load(themes_dir: &Path) -> Self {
        let (custom, errors) = scan(themes_dir);
        Self {
            dir: themes_dir.to_owned(),
            custom,
            errors,
        }
    }

    /// Rescans the folder, for `reload-themes` and Noctalia updates.
    pub fn reload(&mut self) {
        let (custom, errors) = scan(&self.dir);
        self.custom = custom;
        self.errors = errors;
    }

    /// File names of the user's palettes, sorted. Show them with
    /// [`display_name`].
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.custom
            .iter()
            .map(|theme| theme.filename.clone())
            .collect()
    }

    /// One line per theme file that could not be used: `<file>: <reason>`.
    #[must_use]
    pub fn errors(&self) -> &[String] {
        &self.errors
    }

    /// The palette for a choice. A named theme that is gone falls back to
    /// the built-in one matching the system appearance.
    #[must_use]
    pub fn resolve(&self, choice: &ThemeChoice, system_dark: bool) -> &Palette {
        const DARK: Palette = Palette::dark();
        const LIGHT: Palette = Palette::light();
        let builtin = |dark| if dark { &DARK } else { &LIGHT };
        match choice {
            ThemeChoice::System => builtin(system_dark),
            ThemeChoice::Light => builtin(false),
            ThemeChoice::Dark => builtin(true),
            ThemeChoice::Named(name) => self
                .custom
                .iter()
                .find(|theme| &theme.filename == name)
                .map_or_else(|| builtin(system_dark), |theme| &theme.palette),
        }
    }
}

/// A theme file's name without `.json`, for display.
#[must_use]
pub fn display_name(filename: &str) -> &str {
    fastframe_theme::display_name(filename)
}

fn scan(dir: &Path) -> (Vec<CustomTheme<Palette>>, Vec<String>) {
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (themes, errors),
        Err(error) => {
            errors.push(format!("themes folder: {error}"));
            return (themes, errors);
        }
    };
    let mut filenames: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        })
        .collect();
    filenames.sort();
    filenames.truncate(fastframe_theme::MAX_THEMES);
    for filename in filenames {
        match read_theme(&dir.join(&filename)) {
            Ok(palette) => themes.push(CustomTheme { filename, palette }),
            Err(message) => errors.push(format!("{filename}: {message}")),
        }
    }
    (themes, errors)
}

fn read_theme(path: &Path) -> Result<Palette, String> {
    use std::io::Read as _;

    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("not a regular file".to_owned());
    }
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(fastframe_theme::MAX_FILE_BYTES + 1)
                .read_to_string(&mut text)
        })
        .map_err(|error| error.to_string())?;
    if text.len() as u64 > fastframe_theme::MAX_FILE_BYTES {
        return Err("theme exceeds the 64 KiB file limit".to_owned());
    }
    fastframe_theme::parse_palette(&text)
}

/// Spacing scale, in multiples of 4.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 20.0;
    pub const XXL: f32 = 24.0;
}

/// Corner radii: controls, cards and rows, windows and sheets.
pub mod radius {
    pub const CONTROL: u8 = 6;
    pub const CARD: u8 = 8;
    pub const WINDOW: u8 = 10;
}

/// Fixed dimensions, in points.
pub mod size {
    pub const WINDOW_MARGIN: f32 = 20.0;
    pub const CONTROL_HEIGHT: f32 = 28.0;
    pub const SIDEBAR_ROW_HEIGHT: f32 = 32.0;
    pub const LIST_ROW_HEIGHT: f32 = 44.0;
    pub const SIDEBAR_DEFAULT: f32 = 240.0;
    pub const SIDEBAR_MIN: f32 = 200.0;
    pub const SIDEBAR_MAX: f32 = 320.0;
    pub const WINDOW_DEFAULT: [f32; 2] = [960.0, 640.0];
    pub const WINDOW_MIN: [f32; 2] = [760.0, 520.0];
    pub const SHEET_WIDTH: f32 = 560.0;
    pub const DIALOG_MAX: [f32; 2] = [880.0, 620.0];
    pub const DIALOG_SIDEBAR: f32 = 200.0;
    /// A sheet is at most this fraction of the window tall.
    pub const SHEET_MAX_HEIGHT: f32 = 0.8;
    pub const SWITCH: [f32; 2] = [40.0, 22.0];
    /// The combo of a setting row.
    pub const SETTING_COMBO: f32 = 200.0;
    /// The number field of a setting row.
    pub const SETTING_FIELD: f32 = 72.0;
    /// The slot after a number field where its unit goes, so every field of a
    /// section ends at the same edge.
    pub const SETTING_UNIT: f32 = 40.0;
    pub const ICON: f32 = 16.0;
    pub const ICON_LARGE: f32 = 40.0;
}

/// 26 pt bold: the welcome title.
#[must_use]
pub fn large_title() -> FontId {
    Weight::Bold.font_id(26.0)
}

/// 17 pt semibold: detail and sheet titles.
#[must_use]
pub fn title2() -> FontId {
    Weight::SemiBold.font_id(17.0)
}

/// 15 pt semibold: section titles.
#[must_use]
pub fn title3() -> FontId {
    Weight::SemiBold.font_id(15.0)
}

/// 13 pt semibold: row labels and buttons.
#[must_use]
pub fn headline() -> FontId {
    Weight::SemiBold.font_id(13.0)
}

/// 13 pt regular: text.
#[must_use]
pub fn body() -> FontId {
    Weight::Regular.font_id(13.0)
}

/// 12 pt regular: supporting text.
#[must_use]
pub fn callout() -> FontId {
    Weight::Regular.font_id(12.0)
}

/// 11 pt regular: row metadata.
#[must_use]
pub fn subheadline() -> FontId {
    Weight::Regular.font_id(11.0)
}

/// 10 pt regular: the minimum, technical captions only.
#[must_use]
pub fn footnote() -> FontId {
    Weight::Regular.font_id(10.0)
}

/// 12 pt monospace: logs and rules.
#[must_use]
pub fn monospace() -> FontId {
    FontId::monospace(12.0)
}

static TEXT_RENDERING: std::sync::Mutex<Option<fastframe_text::TextRendering>> =
    std::sync::Mutex::new(None);

/// The desktop's font rendering settings, read once and then kept current by
/// [`follow_text_rendering`]. `detect` blocks on D-Bus (about a second when
/// the portal does not answer), so the first call must be outside a tokio
/// runtime.
fn text_rendering() -> fastframe_text::TextRendering {
    *TEXT_RENDERING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(|| {
            if cfg!(test) {
                fastframe_text::TextRendering::platform_default()
            } else {
                fastframe_text::detect()
            }
        })
}

fn install_fonts(ctx: &egui::Context, rendering: fastframe_text::TextRendering) {
    let mut fonts = FontSetup::default().definitions();
    rendering.apply_to(&mut fonts);
    ctx.set_fonts(fonts);
}

static FOLLOWING: std::sync::Mutex<Option<egui::Context>> = std::sync::Mutex::new(None);

/// Follows live changes to the desktop's font settings: reinstalls the fonts
/// and reapplies the palette on the most recent `ctx` passed here. Starts one
/// thread per process; without a desktop portal the settings read at start
/// stay.
pub fn follow_text_rendering(ctx: &egui::Context) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    *FOLLOWING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ctx.clone());
    STARTED.call_once(|| {
        let watched = fastframe_text::watch::watch(text_rendering(), |rendering| {
            *TEXT_RENDERING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rendering);
            let ctx = FOLLOWING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(ctx) = ctx {
                install_fonts(&ctx, rendering);
                apply(&ctx, &palette(&ctx));
                ctx.request_repaint();
            }
        });
        if let Err(error) = watched {
            log::debug!("not following the desktop's font settings: {error}");
        }
    });
}

fn assets_flag() -> egui::Id {
    egui::Id::new("syncfast-theme-assets")
}

fn palette_key() -> egui::Id {
    egui::Id::new("syncfast-theme-palette")
}

/// Registers fonts, image loaders and icons once per context, then applies
/// `palette`. Safe to call on every theme change. Call it before the first
/// frame (the app creator): egui binds new fonts on the next pass, and
/// drawing a widget before that panics on the missing font family.
pub fn install(ctx: &egui::Context, palette: &Palette) {
    install_with(ctx, palette, text_rendering());
}

/// [`install`] with the font rendering given, which then becomes the current
/// one. For tests that must not ask the desktop.
pub fn install_with(
    ctx: &egui::Context,
    palette: &Palette,
    rendering: fastframe_text::TextRendering,
) {
    *TEXT_RENDERING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rendering);
    let first = ctx.data_mut(|data| {
        let seen = data.get_temp::<bool>(assets_flag()).unwrap_or(false);
        data.insert_temp(assets_flag(), true);
        !seen
    });
    if first {
        install_fonts(ctx, rendering);
        egui_extras::install_image_loaders(ctx);
        fastframe_icons::install::<Icon>(ctx);
    }
    apply(ctx, palette);
}

/// Applies `palette` to egui's style and remembers it for [`palette`].
pub fn apply(ctx: &egui::Context, palette: &Palette) {
    let mut style = (*ctx.global_style()).clone();
    apply_to_style(&mut style, palette);
    ctx.set_global_style(style);
    ctx.data_mut(|data| data.insert_temp(palette_key(), *palette));
}

/// The palette last applied to this context. Dark until one is applied.
#[must_use]
pub fn palette(ctx: &egui::Context) -> Palette {
    ctx.data(|data| data.get_temp(palette_key()))
        .unwrap_or_else(Palette::dark)
}

fn apply_to_style(style: &mut egui::Style, palette: &Palette) {
    let visuals = &mut style.visuals;
    *visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.dark_mode = palette.dark;
    text_rendering().apply_to_visuals(visuals);
    visuals.panel_fill = palette.panel;
    visuals.window_fill = palette.overlay;
    visuals.extreme_bg_color = palette.surface;
    visuals.faint_bg_color = palette.surface;
    visuals.code_bg_color = palette.surface;
    visuals.override_text_color = Some(palette.text);
    visuals.weak_text_color = Some(palette.secondary);
    visuals.hyperlink_color = palette.text;
    visuals.selection.bg_fill = palette.accent.gamma_multiply(0.35);
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.window_stroke = Stroke::new(1.0, palette.outline);
    visuals.window_corner_radius = CornerRadius::same(radius::WINDOW);
    visuals.menu_corner_radius = CornerRadius::same(radius::CONTROL);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 24,
        spread: 0,
        color: palette.shadow,
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: palette.shadow,
    };
    let corner = CornerRadius::same(radius::CONTROL);
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = corner;
        widget.bg_stroke = Stroke::NONE;
        widget.fg_stroke = Stroke::new(1.0, palette.text);
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.corner_radius = corner;
    visuals.widgets.noninteractive.bg_fill = palette.panel;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.outline);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);
    visuals.widgets.inactive.bg_fill = palette.surface;
    visuals.widgets.inactive.weak_bg_fill = palette.surface;
    visuals.widgets.hovered.bg_fill = palette.surface_hover;
    visuals.widgets.hovered.weak_bg_fill = palette.surface_hover;
    visuals.widgets.active.bg_fill = palette.surface_active;
    visuals.widgets.active.weak_bg_fill = palette.surface_active;
    visuals.widgets.open.bg_fill = palette.surface_hover;
    visuals.widgets.open.weak_bg_fill = palette.surface_hover;
    visuals.text_cursor.stroke = Stroke::new(2.0, palette.accent);
    visuals.striped = false;
    visuals.slider_trailing_fill = true;
    visuals.handle_shape = egui::style::HandleShape::Circle;

    style.text_styles = [
        (TextStyle::Small, subheadline()),
        (TextStyle::Body, body()),
        (TextStyle::Button, headline()),
        (TextStyle::Heading, title2()),
        (TextStyle::Monospace, monospace()),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(space::S, 6.0);
    style.spacing.button_padding = Vec2::new(space::M, 6.0);
    style.spacing.interact_size = Vec2::new(40.0, size::CONTROL_HEIGHT);
    style.spacing.menu_margin = egui::Margin::same(6);
    style.spacing.window_margin = egui::Margin::same(20);
    style.spacing.scroll = egui::style::ScrollStyle {
        bar_width: 8.0,
        floating_width: 6.0,
        floating_allocated_width: 0.0,
        handle_min_length: 28.0,
        bar_inner_margin: 3.0,
        bar_outer_margin: 2.0,
        dormant_background_opacity: 0.0,
        dormant_handle_opacity: 0.0,
        active_background_opacity: 0.0,
        active_handle_opacity: 0.55,
        interact_handle_opacity: 0.85,
        foreground_color: true,
        ..egui::style::ScrollStyle::floating()
    };
    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.4;
    style.url_in_tooltip = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_choice_round_trips_as_its_string() {
        let cases = [
            (ThemeChoice::System, "\"system\""),
            (ThemeChoice::Light, "\"light\""),
            (ThemeChoice::Dark, "\"dark\""),
            (
                ThemeChoice::Named("noctalia.json".into()),
                "\"noctalia.json\"",
            ),
        ];
        for (choice, json) in cases {
            assert_eq!(serde_json::to_string(&choice).unwrap(), json);
            assert_eq!(serde_json::from_str::<ThemeChoice>(json).unwrap(), choice);
        }
    }

    #[test]
    fn a_palette_file_overrides_only_the_colours_it_names() {
        let palette: Palette =
            fastframe_theme::parse_palette(r##"{"base":"light","colors":{"accent":"#102030"}}"##)
                .unwrap();

        assert!(!palette.dark);
        assert_eq!(palette.accent, Color32::from_rgb(0x10, 0x20, 0x30));
        assert_eq!(palette.window, Palette::light().window);
    }

    #[test]
    fn a_file_without_overlay_derives_it_from_its_window() {
        let palette: Palette =
            fastframe_theme::parse_palette(r##"{"colors":{"window":"#101010"}}"##).unwrap();

        assert_ne!(palette.overlay, Palette::dark().overlay);
        assert_eq!(palette.window, Color32::from_rgb(0x10, 0x10, 0x10));
    }
}
