//! The window's theme, kept in `gui.json` next to what else the window
//! remembers.

use std::path::PathBuf;

use fastframe_instance::Slot;
use serde::Serialize;
use serde_json::{Map, Value};

use super::{OpError, Ops};
use crate::notify::SLOT;

const BUILTIN: [&str; 3] = ["system", "light", "dark"];
const KEY: &str = "theme";

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
    fn gui_file(&self) -> PathBuf {
        self.paths.state_dir().join("gui.json")
    }

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
        let current = tokio::fs::read(self.gui_file())
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|json| json.get(KEY).and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_else(|| BUILTIN[0].to_owned());
        ThemeInfo {
            current,
            available: self.available_themes().await,
        }
    }

    /// Sets the window's theme and tells the running window.
    ///
    /// # Errors
    /// `theme` is not one of [`Ops::theme`]'s `available`, or the file
    /// cannot be written.
    pub async fn set_theme(&self, theme: &str) -> Result<ThemeSet, OpError> {
        let available = self.available_themes().await;
        if !available.iter().any(|name| name == theme) {
            return Err(OpError::Invalid(format!(
                "unknown theme \"{theme}\"; choose one of: {}",
                available.join(", ")
            )));
        }
        let path = self.gui_file();
        let mut json = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice::<Map<String, Value>>(&bytes).unwrap_or_default(),
            Err(_) => Map::new(),
        };
        json.insert(KEY.to_owned(), Value::String(theme.to_owned()));
        let mut text = serde_json::to_string_pretty(&json)
            .map_err(|err| OpError::Failed(format!("cannot encode the window state: {err}")))?;
        text.push('\n');
        let target = path.clone();
        tokio::task::spawn_blocking(move || fastframe_file::write(&target, text))
            .await
            .unwrap_or_else(|join| Err(std::io::Error::other(join)))
            .map_err(|err| OpError::Failed(format!("cannot write {}: {err}", path.display())))?;
        let window_notified = Slot::new(SLOT).send("reload-themes").is_ok();
        Ok(ThemeSet {
            theme: theme.to_owned(),
            window_notified,
        })
    }
}
