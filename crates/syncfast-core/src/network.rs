//! Whether the machine is online, behind a trait so rounds can be tested
//! without a bus.

use std::pin::Pin;
use std::time::Duration;

const NM_SERVICE: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
/// `NM_STATE_CONNECTED_GLOBAL`.
const NM_CONNECTED_GLOBAL: u32 = 70;
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
    let connection = zbus::Connection::system().await?;
    let proxy = zbus::Proxy::new(
        &connection,
        NM_SERVICE,
        NM_PATH,
        "org.freedesktop.NetworkManager",
    )
    .await?;
    proxy.get_property("State").await
}
