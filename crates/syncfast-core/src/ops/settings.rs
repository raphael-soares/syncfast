//! Reading and changing the behavior settings, for the CLI and the MCP
//! server. They cannot lower a safety setting: that is the user's call, in
//! the app.

use serde::Serialize;

use super::{OpError, Ops};
use crate::behavior::Behavior;
use crate::config::{Config, SettingScope};
use crate::settings::{APP_KEYS, KEYS, Origin, lowered_safety};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingView {
    pub key: &'static str,
    pub value: serde_json::Value,
    /// `builtin`, `defaults` or `sync`.
    pub origin: &'static str,
    /// What the key takes after `settings reset`.
    pub default: serde_json::Value,
    /// Whether the key matters for the sync's mode.
    pub applies: bool,
}

/// `defaults` or one sync, with every key it can set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingsView {
    /// `defaults`, or the id of the sync.
    pub scope: String,
    pub settings: Vec<SettingView>,
    /// The keys whose value is less safe than the built-in default.
    pub lowered_safety: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SettingChanged {
    pub scope: String,
    pub setting: SettingView,
    /// What `apply` could not do. The config is saved regardless.
    pub warnings: Vec<String>,
}

const DEFAULTS_SCOPE: &str = "defaults";

impl Ops {
    /// Every setting of `[defaults]`, or of the sync `sync`.
    ///
    /// # Errors
    /// The config has a global problem, or no sync has the id.
    pub async fn settings(&self, sync: Option<&str>) -> Result<SettingsView, OpError> {
        let config = self.load_config().await?;
        Ok(match sync {
            None => defaults_view(&config),
            Some(id) => {
                let pair = config
                    .pairs()
                    .iter()
                    .find(|pair| pair.id == id)
                    .ok_or_else(|| OpError::NotFound(id.to_owned()))?;
                let effective = config.effective(pair);
                let defaults = config.defaults();
                SettingsView {
                    scope: id.to_owned(),
                    settings: KEYS
                        .iter()
                        .map(|def| SettingView {
                            key: def.name,
                            value: def.get(&effective.behavior).to_json(),
                            origin: effective.origin(def.name).name(),
                            default: def.get(&defaults).to_json(),
                            applies: true,
                        })
                        .collect(),
                    lowered_safety: lowered_safety(&effective.behavior),
                }
            }
        })
    }

    /// Sets `key` for `sync`, or in `[defaults]` without it.
    ///
    /// # Errors
    /// The key, the value or the combination is not valid, no sync has the
    /// id, the change lowers a safety setting, or the config cannot be saved.
    pub async fn set_setting(
        &self,
        sync: Option<&str>,
        key: &str,
        value: &str,
    ) -> Result<SettingChanged, OpError> {
        let scope = scope_of(sync);
        Config::edit(&self.paths, |config| {
            let mut next = config.clone();
            next.set_setting(&scope, key, value)?;
            allow(config, &next)?;
            *config = next;
            Ok::<(), OpError>(())
        })
        .await??;
        self.changed(sync, key).await
    }

    /// Removes `key` from `sync`, or from `[defaults]` without it.
    ///
    /// # Errors
    /// The key does not exist, no sync has the id, the value it falls back
    /// to is not valid or is less safe, or the config cannot be saved.
    pub async fn reset_setting(
        &self,
        sync: Option<&str>,
        key: &str,
    ) -> Result<SettingChanged, OpError> {
        let scope = scope_of(sync);
        Config::edit(&self.paths, |config| {
            let mut next = config.clone();
            next.reset_setting(&scope, key)?;
            allow(config, &next)?;
            *config = next;
            Ok::<(), OpError>(())
        })
        .await??;
        self.changed(sync, key).await
    }

    async fn changed(&self, sync: Option<&str>, key: &str) -> Result<SettingChanged, OpError> {
        let warnings = self.reapply(&[]).await;
        let view = self.settings(sync).await?;
        let setting = view
            .settings
            .into_iter()
            .find(|setting| setting.key == key)
            .ok_or_else(|| OpError::Failed(format!("the setting \"{key}\" is not listed")))?;
        Ok(SettingChanged {
            scope: view.scope,
            setting,
            warnings,
        })
    }
}

fn scope_of(sync: Option<&str>) -> SettingScope {
    sync.map_or(SettingScope::Defaults, |id| {
        SettingScope::Sync(id.to_owned())
    })
}

fn defaults_view(config: &Config) -> SettingsView {
    let defaults = config.defaults();
    SettingsView {
        scope: DEFAULTS_SCOPE.to_owned(),
        settings: KEYS
            .iter()
            .filter(|def| !def.per_sync_only)
            .chain(&APP_KEYS)
            .map(|def| {
                let origin = if config.defaults_overrides().contains(def.name) {
                    Origin::Defaults
                } else {
                    Origin::Builtin
                };
                SettingView {
                    key: def.name,
                    value: def.get(&defaults).to_json(),
                    origin: origin.name(),
                    default: def.builtin().to_json(),
                    applies: true,
                }
            })
            .collect(),
        lowered_safety: lowered_safety(&defaults),
    }
}

/// Refuses a change that makes `[defaults]` or any sync less safe than it
/// was.
fn allow(before: &Config, after: &Config) -> Result<(), OpError> {
    let (defaults_before, defaults_after) = (before.defaults(), after.defaults());
    let mut resolved = vec![(defaults_before.clone(), defaults_after.clone())];
    for (old, new) in before.pairs().iter().zip(after.pairs()) {
        resolved.push((
            Behavior::for_pair(&defaults_before, old),
            Behavior::for_pair(&defaults_after, new),
        ));
    }
    let lowered: Vec<&str> = KEYS
        .iter()
        .filter(|def| resolved.iter().any(|(old, new)| def.lowers(old, new)))
        .map(|def| def.name)
        .collect();
    if lowered.is_empty() {
        Ok(())
    } else {
        Err(OpError::NeedsUser(lowered.join(", ")))
    }
}
