//! The status service of `syncfast watch` over a private session bus, asked
//! the way the Nautilus extension asks. Skipped when `dbus-daemon` is not
//! installed.

#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::Bed;
use futures_util::StreamExt as _;
use syncfast_core::network::BoxFuture;
use syncfast_core::state::{State, try_lock_pair};
use syncfast_core::status_bus::{BUS_NAME, Bus, OBJECT_PATH};
use syncfast_core::watch::{Trigger, WatchEnv, Watcher};
use tokio::sync::oneshot;

struct PrivateBus {
    daemon: Child,
    address: String,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let mut daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(daemon.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        Some(Self {
            daemon,
            address: address.trim().to_owned(),
        })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Trigger for Recorder {
    fn start<'a>(&'a self, pair_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.0.lock().unwrap().push(pair_id.to_owned());
            Ok(())
        })
    }

    fn start_backup<'a>(&'a self, _backup_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("this test starts no backups".to_owned()) })
    }
}

async fn client(bus: &PrivateBus) -> zbus::Proxy<'static> {
    let connection = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .build()
        .await
        .unwrap();
    zbus::Proxy::new(&connection, BUS_NAME, OBJECT_PATH, BUS_NAME)
        .await
        .unwrap()
}

async fn roots(proxy: &zbus::Proxy<'_>) -> Vec<(String, String)> {
    proxy.call("Roots", &()).await.unwrap()
}

async fn conflicts(proxy: &zbus::Proxy<'_>) -> Vec<String> {
    proxy.call("Conflicts", &()).await.unwrap()
}

async fn sync_now(proxy: &zbus::Proxy<'_>, path: &str) -> bool {
    proxy.call("SyncNow", &(path,)).await.unwrap()
}

/// Asks `Roots` until the service has published the synced folder; empty
/// when it never does.
async fn wait_for_roots(proxy: &zbus::Proxy<'_>) -> Vec<(String, String)> {
    for _ in 0..100 {
        if let Ok(found) = proxy
            .call::<_, _, Vec<(String, String)>>("Roots", &())
            .await
            && !found.is_empty()
        {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Vec::new()
}

/// Asks `Roots` until the one root reads `state`, since the file events
/// make the watcher publish `pending` first; the last answer otherwise.
async fn wait_for_state(proxy: &zbus::Proxy<'_>, state: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for _ in 0..100 {
        found = roots(proxy).await;
        if found.iter().all(|(_, read)| read == state) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    found
}

#[tokio::test]
async fn roots_conflicts_sync_now_and_changed_over_the_bus() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let local = bed.local("docs");
    State::default().save(&bed.paths, "docs").await.unwrap();
    let recorder = Arc::new(Recorder::default());
    let mut env = WatchEnv::new(bed.paths.clone(), bed.rclone.clone(), recorder.clone());
    env.wake_sources = false;
    env.status_bus = Some(Bus::Address(bus.address.clone()));
    let watcher = Watcher::start(env).await.unwrap();
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        watcher
            .run(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let proxy = client(&bus).await;
    let root = local.to_str().unwrap().to_owned();

    let listed = wait_for_roots(&proxy).await;
    let none_yet = conflicts(&proxy).await;
    let inside = sync_now(&proxy, &format!("{root}/notes/a.txt")).await;
    let the_root = sync_now(&proxy, &root).await;
    let sibling = sync_now(&proxy, &format!("{root}2/a.txt")).await;
    let elsewhere = sync_now(&proxy, "/nowhere/a.txt").await;

    assert_eq!(listed, [(root.clone(), "synced".to_owned())]);
    assert!(none_yet.is_empty());
    assert_eq!(
        [inside, the_root, sibling, elsewhere],
        [true, true, false, false]
    );
    assert_eq!(*recorder.0.lock().unwrap(), ["docs", "docs"]);

    let mut changed = proxy.receive_signal("Changed").await.unwrap();
    fs::write(local.join("a.txt"), "mine").unwrap();
    fs::write(local.join("a.txt.conflict1"), "theirs").unwrap();
    State {
        conflicts_pending: 1,
        ..State::default()
    }
    .save(&bed.paths, "docs")
    .await
    .unwrap();
    let signalled = tokio::time::timeout(Duration::from_secs(10), changed.next())
        .await
        .is_ok();

    assert!(signalled);
    assert_eq!(
        wait_for_state(&proxy, "conflict").await,
        [(root.clone(), "conflict".to_owned())]
    );
    assert_eq!(
        conflicts(&proxy).await,
        [format!("{root}/a.txt"), format!("{root}/a.txt.conflict1")]
    );

    let mut changed = proxy.receive_signal("Changed").await.unwrap();
    let held = try_lock_pair(&bed.paths, "docs").unwrap().unwrap();
    State::default().save(&bed.paths, "docs").await.unwrap();
    let signalled = tokio::time::timeout(Duration::from_secs(10), changed.next())
        .await
        .is_ok();

    assert!(signalled);
    assert_eq!(
        wait_for_state(&proxy, "syncing").await,
        [(root, "syncing".to_owned())]
    );
    assert!(conflicts(&proxy).await.is_empty());
    drop(held);
    let _ = stop.send(());
    task.await.unwrap();
}
