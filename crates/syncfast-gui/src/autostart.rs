//! Starting syncfast when the user logs in, asked of the desktop through the
//! Background portal. Only the Flatpak install asks: outside it there is no
//! portal to make the entry.

use std::collections::HashMap;

use futures_util::StreamExt as _;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const DESTINATION: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const BACKGROUND: &str = "org.freedesktop.portal.Background";
const REQUEST: &str = "org.freedesktop.portal.Request";
const REASON: &str = "Start syncfast hidden when you log in";
const COMMANDLINE: [&str; 2] = ["syncfast-gui", "--start-hidden"];
const TOKEN: &str = "syncfast_autostart";

#[must_use]
pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some_and(|id| !id.is_empty())
}

#[must_use]
pub fn on_hyprland() -> bool {
    is_hyprland(std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref())
}

fn is_hyprland(desktop: Option<&str>) -> bool {
    desktop.is_some_and(|desktop| desktop.contains("Hyprland"))
}

/// What to ask the desktop now, given what the config wants, what it last
/// agreed to and what was already asked in this run. A refused request is
/// not repeated until the config wants something else.
#[must_use]
pub fn to_ask(in_flatpak: bool, wanted: bool, applied: bool, asked: Option<bool>) -> Option<bool> {
    (in_flatpak && wanted != applied && asked != Some(wanted)).then_some(wanted)
}

/// Asks the session's portal to start syncfast at login (`enabled`) or not.
/// Returns whether the desktop granted exactly that.
///
/// # Errors
/// No session bus or no Background portal on it, or an answer that cannot be
/// read.
pub async fn apply(enabled: bool) -> zbus::Result<bool> {
    let connection = zbus::Connection::session().await?;
    let sender = connection
        .unique_name()
        .map(|name| name.as_str().trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    request(&connection, DESTINATION, &sender, enabled).await
}

/// The portal call itself. `sender` is the connection's unique name as the
/// portal spells it in request paths: no colon, dots as underscores.
async fn request(
    connection: &zbus::Connection,
    destination: &str,
    sender: &str,
    enabled: bool,
) -> zbus::Result<bool> {
    let expected = format!("{PORTAL_PATH}/request/{sender}/{TOKEN}");
    // Subscribe before asking: the answer can arrive before the call returns.
    let mut answers = responses(connection, destination, &expected).await?;
    let portal = zbus::Proxy::new(connection, destination, PORTAL_PATH, BACKGROUND).await?;
    let options: HashMap<&str, Value<'_>> = HashMap::from([
        ("handle_token", Value::from(TOKEN)),
        ("reason", Value::from(REASON)),
        ("autostart", Value::from(enabled)),
        ("commandline", Value::from(COMMANDLINE.to_vec())),
        ("dbus-activatable", Value::from(false)),
    ]);
    let handle: OwnedObjectPath = portal.call("RequestBackground", &("", options)).await?;
    if handle.as_str() != expected {
        answers = responses(connection, destination, handle.as_str()).await?;
    }
    let Some(message) = answers.next().await else {
        return Ok(false);
    };
    let (code, results): (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
    let granted = matches!(
        results.get("autostart").map(|value| &**value),
        Some(Value::Bool(granted)) if *granted == enabled
    );
    Ok(code == 0 && granted)
}

async fn responses(
    connection: &zbus::Connection,
    destination: &str,
    path: &str,
) -> zbus::Result<zbus::proxy::SignalStream<'static>> {
    zbus::Proxy::new(connection, destination.to_owned(), path.to_owned(), REQUEST)
        .await?
        .receive_signal("Response")
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct Call {
        parent_window: String,
        reason: String,
        autostart: bool,
        commandline: Vec<String>,
        dbus_activatable: bool,
    }

    struct FakePortal {
        calls: Arc<Mutex<Vec<Call>>>,
        code: u32,
        autostart: bool,
    }

    #[zbus::interface(name = "org.freedesktop.portal.Background")]
    impl FakePortal {
        #[allow(clippy::needless_pass_by_value)]
        fn request_background(
            &self,
            #[zbus(connection)] connection: &zbus::Connection,
            parent_window: String,
            options: HashMap<String, OwnedValue>,
        ) -> OwnedObjectPath {
            let text = |key: &str| match options.get(key).map(|value| &**value) {
                Some(Value::Str(text)) => text.to_string(),
                _ => String::new(),
            };
            let flag = |key: &str| {
                matches!(
                    options.get(key).map(|value| &**value),
                    Some(Value::Bool(true))
                )
            };
            let commandline = match options.get("commandline").map(|value| &**value) {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(|item| match item {
                        Value::Str(text) => Some(text.to_string()),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            self.calls.lock().unwrap().push(Call {
                parent_window,
                reason: text("reason"),
                autostart: flag("autostart"),
                commandline,
                dbus_activatable: flag("dbus-activatable"),
            });
            let path = format!("{PORTAL_PATH}/request/test/{}", text("handle_token"));
            let answer = (
                self.code,
                HashMap::from([("autostart", Value::from(self.autostart))]),
            );
            let connection = connection.clone();
            let signal_path = path.clone();
            tokio::spawn(async move {
                connection
                    .emit_signal(None::<&str>, signal_path, REQUEST, "Response", &answer)
                    .await
                    .unwrap();
            });
            OwnedObjectPath::try_from(path).unwrap()
        }
    }

    async fn ask(code: u32, answered: bool, enabled: bool) -> (bool, Vec<Call>) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let portal = FakePortal {
            calls: Arc::clone(&calls),
            code,
            autostart: answered,
        };
        let (server_end, client_end) = tokio::net::UnixStream::pair().unwrap();
        let server = zbus::connection::Builder::unix_stream(server_end)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(PORTAL_PATH, portal)
            .unwrap()
            .build();
        let client = zbus::connection::Builder::unix_stream(client_end)
            .p2p()
            .build();
        let (_server, client) = tokio::try_join!(server, client).unwrap();

        let granted = request(&client, "org.example.Portal", "test", enabled)
            .await
            .unwrap();
        let seen = calls.lock().unwrap().clone();
        (granted, seen)
    }

    #[tokio::test]
    async fn the_request_carries_the_reason_the_command_line_and_the_wanted_value() {
        let (_, calls) = ask(0, true, true).await;

        assert_eq!(
            calls,
            [Call {
                parent_window: String::new(),
                reason: "Start syncfast hidden when you log in".into(),
                autostart: true,
                commandline: vec!["syncfast-gui".into(), "--start-hidden".into()],
                dbus_activatable: false,
            }]
        );
    }

    #[tokio::test]
    async fn an_answer_that_grants_what_was_asked_is_granted() {
        assert!(ask(0, true, true).await.0);
        assert!(ask(0, false, false).await.0);
    }

    #[tokio::test]
    async fn a_denied_answer_is_not_granted() {
        assert!(!ask(1, false, true).await.0);
        assert!(!ask(2, true, true).await.0);
    }

    #[tokio::test]
    async fn an_answer_with_another_autostart_value_is_not_granted() {
        assert!(!ask(0, false, true).await.0);
    }

    #[test]
    fn it_asks_only_when_the_wish_differs_from_what_the_desktop_last_agreed_to() {
        assert_eq!(to_ask(true, true, false, None), Some(true));
        assert_eq!(to_ask(true, false, true, None), Some(false));
        assert_eq!(to_ask(true, true, true, None), None);
        assert_eq!(to_ask(true, false, false, None), None);
    }

    #[test]
    fn it_does_not_ask_again_after_asking_for_the_same_thing() {
        assert_eq!(to_ask(true, true, false, Some(true)), None);
        assert_eq!(to_ask(true, false, true, Some(true)), Some(false));
    }

    #[test]
    fn outside_the_flatpak_it_never_asks() {
        assert_eq!(to_ask(false, true, false, None), None);
    }

    #[test]
    fn hyprland_is_found_in_the_desktop_name() {
        assert!(is_hyprland(Some("Hyprland")));
        assert!(is_hyprland(Some("ubuntu:Hyprland")));
        assert!(!is_hyprland(Some("GNOME")));
        assert!(!is_hyprland(None));
    }
}
