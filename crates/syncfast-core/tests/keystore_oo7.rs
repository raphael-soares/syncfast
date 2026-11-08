//! The backup key in a real Secret Service. It needs an unlocked keyring on
//! the session bus, so it only runs on request:
//! `cargo nextest run -p syncfast-core --test keystore_oo7 --run-ignored all`.

#![allow(clippy::unwrap_used)]

use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::{KeyStore, Oo7KeyStore};

struct Cleanup(String);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let id = std::mem::take(&mut self.0);
        let _ = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(Oo7KeyStore::new().delete(&id))
        })
        .join();
    }
}

#[tokio::test]
#[ignore = "needs an unlocked Secret Service on the session bus"]
async fn a_key_is_stored_read_back_replaced_and_deleted() {
    let store = Oo7KeyStore::new();
    let id = format!("test-{}", std::process::id());
    let _cleanup = Cleanup(id.clone());
    let first = Key::generate().unwrap();
    let second = Key::generate().unwrap();

    store.put(&id, "keystore test", &first).await.unwrap();
    let read = store.get(&id).await.unwrap().unwrap();
    assert_eq!(read.display_groups(), first.display_groups());

    store.put(&id, "keystore test", &second).await.unwrap();
    let read = store.get(&id).await.unwrap().unwrap();
    assert_eq!(read.display_groups(), second.display_groups());

    store.delete(&id).await.unwrap();
    assert!(store.get(&id).await.unwrap().is_none());
    assert!(store.backend().is_some());
}
