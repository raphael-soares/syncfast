//! The window's theme, kept in `[app].theme` of `config.toml`.

use fastframe_instance::Slot;
use serde::Serialize;

use super::{OpError, Ops};
use crate::config::{Config, SettingScope};
use crate::notify::SLOT;

const BUILTIN: [&str; 3] = ["system", "light", "dark"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemeInfo {
    pub current: String,
    /// `system`, `light`, `dark` and the files in the themes folder.
    pub available: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemeSet {
    pub theme: String,
    /// The running window was told to show it now.
    pub window_notified: bool,
}

impl Ops {
    async fn available_themes(&self) -> Vec<String> {
        let mut found: Vec<String> = BUILTIN.iter().map(|name| (*name).to_owned()).collect();
        let Ok(mut entries) = tokio::fs::read_dir(self.paths.config_dir().join("themes")).await
        else {
            return found;
        };
        let mut files = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if std::path::Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                files.push(name);
            }
        }
        files.sort();
        found.extend(files);
        found
    }

    /// The window's theme and the ones it can have.
    pub async fn theme(&self) -> ThemeInfo {
        let current = Config::load(&self.paths).await.map_or_else(
            |_| BUILTIN[0].to_owned(),
            |config| config.defaults().app.theme,
        );
        ThemeInfo {
            current,
            available: self.available_themes().await,
        }
    }

    /// Sets `[app].theme` and tells the running window.
    ///
    /// # Errors
    /// `theme` is not one of [`Ops::theme`]'s `available`, or the config
    /// cannot be saved.
    pub async fn set_theme(&self, theme: &str) -> Result<ThemeSet, OpError> {
        let available = self.available_themes().await;
        if !available.iter().any(|name| name == theme) {
            return Err(OpError::Invalid(format!(
                "unknown theme \"{theme}\"; choose one of: {}",
                available.join(", ")
            )));
        }
        Config::edit(&self.paths, |config| {
            config.set_setting(&SettingScope::Defaults, "app.theme", theme)?;
            Ok::<(), OpError>(())
        })
        .await??;
        let window_notified = Slot::new(SLOT).send("reload-themes").is_ok();
        Ok(ThemeSet {
            theme: theme.to_owned(),
            window_notified,
        })
    }
}
