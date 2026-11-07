//! What the window remembers between runs, in `$XDG_STATE_HOME/syncfast/gui.json`.

use std::path::{Path, PathBuf};

use fastframe_shell::window::Geometry;
use serde::{Deserialize, Serialize};
use syncfast_core::paths::Paths;

use crate::theme::ThemeChoice;

pub const SIDEBAR_MIN_WIDTH: f32 = 200.0;
pub const SIDEBAR_MAX_WIDTH: f32 = 320.0;
pub const SIDEBAR_DEFAULT_WIDTH: f32 = 240.0;

/// [`Geometry`] in a form JSON can hold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowRecord {
    pub size: Option<[f32; 2]>,
    pub position: Option<[f32; 2]>,
    pub maximized: bool,
}

impl From<Geometry> for WindowRecord {
    fn from(geometry: Geometry) -> Self {
        Self {
            size: geometry.size,
            position: geometry.position,
            maximized: geometry.maximized,
        }
    }
}

impl From<WindowRecord> for Geometry {
    fn from(record: WindowRecord) -> Self {
        Self {
            size: record.size,
            position: record.position,
            maximized: record.maximized,
        }
    }
}

/// The panes of Settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsPane {
    #[default]
    Accounts,
    Appearance,
}

/// The sidebar groups the panes of Settings sit under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsGroup {
    Syncs,
    App,
}

impl SettingsGroup {
    pub const ALL: [Self; 2] = [Self::Syncs, Self::App];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Syncs => "Syncs",
            Self::App => "App",
        }
    }
}

impl SettingsPane {
    /// In the order the sidebar lists them, group by group.
    pub const ALL: [Self; 2] = [Self::Accounts, Self::Appearance];

    /// The pane's name, shown in the sidebar and as the title of its content.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Accounts => "Accounts",
            Self::Appearance => "Appearance",
        }
    }

    #[must_use]
    pub fn group(self) -> SettingsGroup {
        match self {
            Self::Accounts => SettingsGroup::Syncs,
            Self::Appearance => SettingsGroup::App,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiState {
    /// The first sync was confirmed or the user skipped the guide: the
    /// welcome screen never comes back.
    pub welcome_done: bool,
    pub selected_pair: Option<String>,
    pub sidebar_width: f32,
    pub theme: ThemeChoice,
    pub window: WindowRecord,
    /// The pane Settings opens on.
    pub settings_pane: SettingsPane,
}

impl Default for GuiState {
    fn default() -> Self {
        Self {
            welcome_done: false,
            selected_pair: None,
            sidebar_width: SIDEBAR_DEFAULT_WIDTH,
            theme: ThemeChoice::System,
            window: WindowRecord::default(),
            settings_pane: SettingsPane::Accounts,
        }
    }
}

#[must_use]
pub fn file(paths: &Paths) -> PathBuf {
    paths.state_dir().join("gui.json")
}

impl GuiState {
    /// A missing or unreadable file gives the defaults: the window then
    /// opens as on a first run instead of refusing to start.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => {
                log::warn!("cannot read {}: {err}", path.display());
                return Self::default();
            }
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut state) => {
                state.sidebar_width = state
                    .sidebar_width
                    .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                state
            }
            Err(err) => {
                log::warn!("{} is not valid, using defaults: {err}", path.display());
                Self::default()
            }
        }
    }

    /// The file's contents. The backend writes them.
    ///
    /// # Errors
    /// The state cannot be encoded as JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_the_defaults() {
        let dir = tempfile::tempdir().unwrap();

        let state = GuiState::load(&dir.path().join("gui.json"));

        assert_eq!(state, GuiState::default());
        assert!(!state.welcome_done);
        assert_eq!(state.theme, ThemeChoice::System);
    }

    #[test]
    fn saved_state_comes_back_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui.json");
        let saved = GuiState {
            welcome_done: true,
            selected_pair: Some("notes".into()),
            sidebar_width: 280.0,
            theme: ThemeChoice::Named("noctalia.json".into()),
            window: WindowRecord {
                size: Some([960.0, 640.0]),
                position: Some([10.0, 20.0]),
                maximized: true,
            },
            settings_pane: SettingsPane::Appearance,
        };
        std::fs::write(&path, saved.to_json().unwrap()).unwrap();

        let loaded = GuiState::load(&path);

        assert_eq!(loaded, saved);
    }

    #[test]
    fn corrupt_file_gives_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui.json");
        std::fs::write(&path, "{ not json").unwrap();

        assert_eq!(GuiState::load(&path), GuiState::default());
    }

    #[test]
    fn sidebar_width_outside_the_allowed_range_is_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui.json");
        std::fs::write(&path, r#"{"sidebar_width": 900.0}"#).unwrap();

        assert!((GuiState::load(&path).sidebar_width - SIDEBAR_MAX_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn theme_is_stored_as_a_plain_string() {
        let state = GuiState {
            theme: ThemeChoice::Dark,
            ..GuiState::default()
        };

        assert!(state.to_json().unwrap().contains(r#""theme": "dark""#));
    }

    #[test]
    fn geometry_survives_the_round_trip_through_a_record() {
        let geometry = Geometry {
            size: Some([800.0, 600.0]),
            position: None,
            maximized: true,
        };

        assert_eq!(Geometry::from(WindowRecord::from(geometry)), geometry);
    }
}
