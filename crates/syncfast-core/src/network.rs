//! Whether the machine is online, behind a trait so rounds can be tested
//! without a bus.

use std::pin::Pin;
use std::time::Duration;

use futures_util::StreamExt as _;
use tokio::sync::mpsc;

use crate::watch::Wake;

const NM_SERVICE: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
/// `NM_STATE_CONNECTED_GLOBAL`.
const NM_CONNECTED_GLOBAL: u32 = 70;
const LOGIND_SERVICE: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_INTERFACE: &str = "org.freedesktop.login1.Manager";
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Network: Send + Sync {
    fn is_online(&self) -> BoxFuture<'_, bool>;
}

/// Reads `State` from `NetworkManager` on the system bus. Without a bus,
/// without `NetworkManager` or when it does not answer in time, the machine
/// counts as online: rclone then fails by itself if there is no network.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkManager;

impl Network for NetworkManager {
    fn is_online(&self) -> BoxFuture<'_, bool> {
        Box::pin(async {
            match tokio::time::timeout(QUERY_TIMEOUT, read_state()).await {
                Ok(Ok(state)) => state >= NM_CONNECTED_GLOBAL,
                Ok(Err(err)) => {
                    log::debug!("NetworkManager state unavailable, assuming online: {err}");
                    true
                }
                Err(_) => {
                    log::debug!("NetworkManager did not answer, assuming online");
                    true
                }
            }
        })
    }
}

async fn read_state() -> zbus::Result<u32> {
    state_on(&zbus::Connection::system().await?).await
}

async fn state_on(connection: &zbus::Connection) -> zbus::Result<u32> {
    network_manager(connection)
        .await?
        .get_property("State")
        .await
}

async fn network_manager(connection: &zbus::Connection) -> zbus::Result<zbus::Proxy<'static>> {
    zbus::Proxy::new(
        connection,
        NM_SERVICE,
        NM_PATH,
        "org.freedesktop.NetworkManager",
    )
    .await
}

/// True when the state went from not online to online.
fn came_online(before: u32, after: u32) -> bool {
    before < NM_CONNECTED_GLOBAL && after >= NM_CONNECTED_GLOBAL
}

/// Turns `PrepareForSleep(false)` (the computer woke) and the network
/// coming back into [`Wake`]s, until the bus goes away or nobody listens.
/// Without a system bus it logs a warning and returns.
pub async fn forward_system_events(wakes: mpsc::Sender<Wake>) {
    if let Err(err) = forward(&wakes).await {
        log::warn!("not watching suspend and network events: {err}");
    }
}

async fn forward(wakes: &mpsc::Sender<Wake>) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;
    let login =
        zbus::Proxy::new(&connection, LOGIND_SERVICE, LOGIND_PATH, LOGIND_INTERFACE).await?;
    let mut sleeping = login.receive_signal("PrepareForSleep").await?;
    let mut states = network_manager(&connection)
        .await?
        .receive_signal("StateChanged")
        .await?;
    let mut state = state_on(&connection).await.unwrap_or(NM_CONNECTED_GLOBAL);
    loop {
        let wake = tokio::select! {
            Some(message) = sleeping.next() => {
                match message.body().deserialize::<bool>() {
                    Ok(false) => Some(Wake::Resumed),
                    _ => None,
                }
            }
            Some(message) = states.next() => {
                let Ok(after) = message.body().deserialize::<u32>() else {
                    continue;
                };
                let wake = came_online(state, after).then_some(Wake::NetworkUp);
                state = after;
                wake
            }
            else => return Ok(()),
        };
        if let Some(wake) = wake
            && wakes.send(wake).await.is_err()
        {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_crossing_into_global_connectivity_counts_as_coming_online() {
        assert!(came_online(20, 70));
        assert!(came_online(60, 70));
        assert!(!came_online(70, 70));
        assert!(!came_online(40, 50));
        assert!(!came_online(70, 20));
    }
}
