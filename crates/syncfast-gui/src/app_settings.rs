//! The theme used to live in `gui.json`; it now lives in `[app].theme` of
//! `config.toml`. This moves it once, when the window opens.

use std::path::Path;

use serde_json::{Map, Value};
use syncfast_core::config::{Config, SettingScope};
use syncfast_core::paths::Paths;

use crate::gui_state;
use crate::theme::ThemeChoice;

const KEY: &str = "theme";
const SETTING: &str = "app.theme";

/// Copies the theme `gui.json` still holds into the config, unless the
/// config already sets one, and drops it from `gui.json`. Returns the theme
/// the window starts with.
#[must_use]
pub fn migrate_theme(paths: &Paths) -> ThemeChoice {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            log::warn!("cannot move the theme into the config: {err}");
            return ThemeChoice::System;
        }
    };
    runtime.block_on(migrate(paths))
}

async fn migrate(paths: &Paths) -> ThemeChoice {
    let file = gui_state::file(paths);
    if let Some(old) = old_theme(&file)
        && old != "system"
        && !copy_to_config(paths, &old).await
    {
        return config_theme(paths).await;
    }
    forget_old_theme(&file);
    config_theme(paths).await
}

/// Whether the old value is dealt with: written, or not needed. `false`
/// keeps it in `gui.json` for the next run.
async fn copy_to_config(paths: &Paths, old: &str) -> bool {
    let copied = Config::edit(paths, |config| {
        if config.defaults_overrides().contains(SETTING) {
            return Ok(());
        }
        config.set_setting(&SettingScope::Defaults, SETTING, old)
    })
    .await;
    match copied {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            log::warn!("the theme in gui.json is not usable: {err}");
            true
        }
        Err(err) => {
            log::warn!("cannot move the theme into the config: {err}");
            false
        }
    }
}

async fn config_theme(paths: &Paths) -> ThemeChoice {
    Config::load(paths)
        .await
        .map_or(ThemeChoice::System, |config| {
            ThemeChoice::from(config.defaults().app.theme)
        })
}

fn read_object(file: &Path) -> Option<Map<String, Value>> {
    let text = std::fs::read_to_string(file).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(object) => Some(object),
        _ => None,
    }
}

fn old_theme(file: &Path) -> Option<String> {
    read_object(file)?.get(KEY)?.as_str().map(str::to_owned)
}

fn forget_old_theme(file: &Path) {
    let Some(mut object) = read_object(file) else {
        return;
    };
    if object.remove(KEY).is_none() {
        return;
    }
    let json = match serde_json::to_string_pretty(&object) {
        Ok(json) => json,
        Err(err) => {
            log::warn!("cannot encode the window state: {err}");
            return;
        }
    };
    if let Err(err) = fastframe_file::write(file, json) {
        log::warn!("cannot drop the theme from {}: {err}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &tempfile::TempDir) -> Paths {
        Paths::new(
            dir.path().join("home"),
            dir.path().join("config"),
            dir.path().join("state"),
            dir.path().join("data"),
            dir.path().join("run"),
        )
    }

    fn write_gui(paths: &Paths, json: &str) {
        let file = gui_state::file(paths);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, json).unwrap();
    }

    fn gui_json(paths: &Paths) -> Value {
        serde_json::from_str(&std::fs::read_to_string(gui_state::file(paths)).unwrap()).unwrap()
    }

    fn config_text(paths: &Paths) -> String {
        std::fs::read_to_string(paths.config_file()).unwrap_or_default()
    }

    #[test]
    fn the_theme_in_gui_json_moves_to_the_config_and_the_rest_stays() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        write_gui(
            &paths,
            r#"{"theme": "dark", "welcome_done": true, "sidebar_width": 280.0}"#,
        );

        let started = migrate_theme(&paths);

        assert_eq!(started, ThemeChoice::Dark);
        assert!(config_text(&paths).contains("theme = \"dark\""));
        let kept = gui_json(&paths);
        assert_eq!(kept.get("theme"), None);
        assert_eq!(kept["welcome_done"], true);
        assert_eq!(kept["sidebar_width"], 280.0);
    }

    #[test]
    fn it_happens_once_and_a_later_choice_in_the_config_wins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        write_gui(&paths, r#"{"theme": "dark"}"#);
        let _ = migrate_theme(&paths);
        write_gui(&paths, r#"{"theme": "light"}"#);

        let started = migrate_theme(&paths);

        assert_eq!(started, ThemeChoice::Dark, "the config already had one");
        assert!(config_text(&paths).contains("theme = \"dark\""));
        assert_eq!(gui_json(&paths).get("theme"), None);
    }

    #[test]
    fn a_system_theme_writes_nothing_to_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        write_gui(&paths, r#"{"theme": "system", "welcome_done": true}"#);

        let started = migrate_theme(&paths);

        assert_eq!(started, ThemeChoice::System);
        assert_eq!(config_text(&paths), "");
        assert_eq!(gui_json(&paths).get("theme"), None);
    }

    #[test]
    fn a_named_theme_file_is_copied_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        write_gui(&paths, r#"{"theme": "noctalia.json"}"#);

        let started = migrate_theme(&paths);

        assert_eq!(started, ThemeChoice::Named("noctalia.json".into()));
        assert!(config_text(&paths).contains("theme = \"noctalia.json\""));
    }

    #[test]
    fn without_gui_json_nothing_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);

        let started = migrate_theme(&paths);

        assert_eq!(started, ThemeChoice::System);
        assert!(!gui_state::file(&paths).exists());
        assert_eq!(config_text(&paths), "");
    }

    #[test]
    fn an_unreadable_config_keeps_the_old_theme_for_the_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        write_gui(&paths, r#"{"theme": "dark"}"#);
        std::fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
        std::fs::write(paths.config_file(), "version = [").unwrap();

        let started = migrate_theme(&paths);

        assert_eq!(started, ThemeChoice::System);
        assert_eq!(gui_json(&paths)["theme"], "dark");
    }
}
