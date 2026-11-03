//! The desktop's light or dark preference, from the settings portal
//! (`org.freedesktop.appearance color-scheme`). winit reports no system
//! theme on most Wayland desktops, so "System" would always stay dark.

use futures_util::StreamExt as _;
use zbus::zvariant::{OwnedValue, Value};

const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

/// Calls `publish` with the preference now and at every change, until the
/// portal's signal stream ends.
///
/// # Errors
/// No session bus, or no settings portal on it.
pub async fn follow(publish: impl Fn(bool)) -> zbus::Result<()> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )
    .await?;
    // Subscribe before reading, so a change in between is not missed.
    let mut changes = proxy
        .receive_signal_with_args("SettingChanged", &[(0, NAMESPACE)])
        .await?;
    // `ReadOne` arrived in version 2 of the interface; `Read` wraps the same
    // answer in one more variant.
    let current: zbus::Result<OwnedValue> = match proxy.call("ReadOne", &(NAMESPACE, KEY)).await {
        Ok(value) => Ok(value),
        Err(_) => proxy.call("Read", &(NAMESPACE, KEY)).await,
    };
    if let Some(value) = current.ok().as_deref().and_then(color_scheme) {
        publish(prefers_dark(value));
    }
    while let Some(message) = changes.next().await {
        let Ok((namespace, key, value)) =
            message.body().deserialize::<(String, String, OwnedValue)>()
        else {
            continue;
        };
        if namespace == NAMESPACE
            && key == KEY
            && let Some(value) = color_scheme(&value)
        {
            publish(prefers_dark(value));
        }
    }
    Ok(())
}

/// The portal's value: 1 prefers dark, 2 prefers light, 0 states no
/// preference, which desktops draw light.
fn prefers_dark(value: u32) -> bool {
    value == 1
}

/// Unwraps the number from the one or two variants it arrives in.
fn color_scheme(value: &Value<'_>) -> Option<u32> {
    match value {
        Value::U32(value) => Some(*value),
        Value::Value(inner) => color_scheme(inner),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_value_one_prefers_dark() {
        assert!(prefers_dark(1));
        assert!(!prefers_dark(2));
        assert!(!prefers_dark(0));
    }

    #[test]
    fn scheme_is_found_inside_nested_variants() {
        let nested = Value::Value(Box::new(Value::Value(Box::new(Value::U32(1)))));

        assert_eq!(color_scheme(&nested), Some(1));
        assert_eq!(color_scheme(&Value::Bool(true)), None);
    }
}
