//! Where the backup keys live (ADR 0009).
//!
//! Production keeps them in the Secret portal's keyring file, or in the
//! host's Secret Service when the portal does not exist. Dev mode may use
//! plain files; tests use memory.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use futures_core::future::BoxFuture;
use tokio::io::AsyncWriteExt;
use tokio::sync::OnceCell;

use super::key::{Key, KeyError};
use crate::flatpak;
use crate::paths::Paths;

const SCHEMA: &str = "io.github.raphael_soares.syncfast.BackupKey";
/// How long a locked keyring may keep a run waiting for its password.
const LOCKED_WAIT: Duration = Duration::from_secs(30);
const KEYSTORE_VAR: &str = "SYNCFAST_KEYSTORE";
const SYSTEMD_VAR: &str = "SYNCFAST_SYSTEMD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBackend {
    /// The encrypted file the Secret portal unlocks for this app.
    Portal,
    /// The host's Secret Service (`org.freedesktop.secrets`).
    HostKeyring,
    /// Files in the data folder: dev mode only, and not secure.
    Plain,
    Memory,
}

pub trait KeyStore: Send + Sync {
    /// # Errors
    /// The key storage cannot be reached or the saved key is unreadable.
    fn get<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>>;

    /// Saves `key` for the backup, replacing a key it already had.
    ///
    /// # Errors
    /// The key storage cannot be reached or refuses the key.
    fn put<'a>(
        &'a self,
        backup_id: &'a str,
        backup_name: &'a str,
        key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>>;

    /// # Errors
    /// The key storage cannot be reached.
    fn delete<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), KeyError>>;

    /// The backend in use, once it is known: [`KeyStore::probe`] or the
    /// first use chose it.
    fn backend(&self) -> Option<KeyBackend>;

    /// Opens the storage without writing anything, so a screen can tell
    /// before a backup is created whether a key can be kept.
    ///
    /// # Errors
    /// No key storage can be reached.
    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>>;
}

/// The keyring item of a backup's key: the attributes it is found by.
fn attributes(backup_id: &str) -> [(&'static str, String); 2] {
    [
        ("xdg:schema", SCHEMA.to_owned()),
        ("backup", backup_id.to_owned()),
    ]
}

fn label(backup_name: &str) -> String {
    format!("syncfast backup key: {backup_name}")
}

enum OpenError {
    /// The portal frontend does not expose `org.freedesktop.portal.Secret`.
    PortalNotFound(String),
    Other(String),
}

trait Vault: Send + Sync {
    async fn get(&self, backup_id: &str) -> Result<Option<Key>, String>;
    async fn put(&self, backup_id: &str, backup_name: &str, key: &Key) -> Result<(), String>;
    async fn delete(&self, backup_id: &str) -> Result<(), String>;
}

trait Opener: Send + Sync {
    type Vault: Vault;
    async fn portal(&self) -> Result<Self::Vault, OpenError>;
    async fn host(&self) -> Result<Self::Vault, String>;
}

struct Chosen<V> {
    backend: KeyBackend,
    primary: V,
    /// The other backend, when it is within reach: only read from and
    /// deleted in, so a key saved before the portal existed is still found.
    secondary: Option<V>,
}

/// The portal first; the host's Secret Service only when there is no
/// portal. Any other portal failure stops here, so a key never lands
/// somewhere the person did not expect.
async fn choose<O: Opener>(opener: &O) -> Result<Chosen<O::Vault>, KeyError> {
    match opener.portal().await {
        Ok(primary) => Ok(Chosen {
            backend: KeyBackend::Portal,
            primary,
            secondary: opener.host().await.ok(),
        }),
        Err(OpenError::PortalNotFound(portal)) => match opener.host().await {
            Ok(primary) => Ok(Chosen {
                backend: KeyBackend::HostKeyring,
                primary,
                secondary: None,
            }),
            Err(host) => Err(KeyError::Unavailable(format!(
                "there is no Secret portal ({portal}) and the Secret Service failed ({host})"
            ))),
        },
        Err(OpenError::Other(detail)) => Err(KeyError::Unavailable(detail)),
    }
}

struct Layered<O: Opener> {
    opener: O,
    chosen: OnceCell<Chosen<O::Vault>>,
}

impl<O: Opener> Layered<O> {
    fn new(opener: O) -> Self {
        Self {
            opener,
            chosen: OnceCell::new(),
        }
    }

    async fn chosen(&self) -> Result<&Chosen<O::Vault>, KeyError> {
        self.chosen.get_or_try_init(|| choose(&self.opener)).await
    }

    async fn probe(&self) -> Result<KeyBackend, KeyError> {
        Ok(self.chosen().await?.backend)
    }

    async fn get(&self, backup_id: &str) -> Result<Option<Key>, KeyError> {
        let chosen = self.chosen().await?;
        if let Some(key) = chosen
            .primary
            .get(backup_id)
            .await
            .map_err(KeyError::Unavailable)?
        {
            return Ok(Some(key));
        }
        match &chosen.secondary {
            Some(other) => other.get(backup_id).await.map_err(KeyError::Unavailable),
            None => Ok(None),
        }
    }

    async fn put(&self, backup_id: &str, backup_name: &str, key: &Key) -> Result<(), KeyError> {
        self.chosen()
            .await?
            .primary
            .put(backup_id, backup_name, key)
            .await
            .map_err(KeyError::Unavailable)
    }

    async fn delete(&self, backup_id: &str) -> Result<(), KeyError> {
        let chosen = self.chosen().await?;
        let primary = chosen.primary.delete(backup_id).await;
        let secondary = match &chosen.secondary {
            Some(other) => other.delete(backup_id).await,
            None => Ok(()),
        };
        primary.and(secondary).map_err(KeyError::Unavailable)
    }

    fn backend(&self) -> Option<KeyBackend> {
        self.chosen.get().map(|chosen| chosen.backend)
    }
}

struct Oo7Vault(oo7::Keyring);

async fn within<T>(call: impl Future<Output = oo7::Result<T>>) -> Result<T, String> {
    match tokio::time::timeout(LOCKED_WAIT, call).await {
        Ok(done) => done.map_err(|err| err.to_string()),
        Err(_) => Err("the keyring is locked or does not answer".to_owned()),
    }
}

impl Vault for Oo7Vault {
    async fn get(&self, backup_id: &str) -> Result<Option<Key>, String> {
        let attributes = attributes(backup_id);
        let items = within(self.0.search_items(&attributes)).await?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let secret = within(item.secret()).await?;
        let text = std::str::from_utf8(secret.as_bytes())
            .map_err(|_| "the saved key is not text".to_owned())?;
        Key::parse(text)
            .map(Some)
            .map_err(|_| "the saved key is not a valid backup key".to_owned())
    }

    async fn put(&self, backup_id: &str, backup_name: &str, key: &Key) -> Result<(), String> {
        within(self.0.create_item(
            &label(backup_name),
            &attributes(backup_id),
            oo7::Secret::text(key.password()),
            true,
        ))
        .await
    }

    async fn delete(&self, backup_id: &str) -> Result<(), String> {
        within(self.0.delete(&attributes(backup_id))).await
    }
}

struct Oo7Opener;

impl Opener for Oo7Opener {
    type Vault = Oo7Vault;

    async fn portal(&self) -> Result<Oo7Vault, OpenError> {
        if flatpak::app_id().is_none() {
            return Err(OpenError::PortalNotFound("not in the sandbox".to_owned()));
        }
        match oo7::Keyring::sandboxed().await {
            Ok(keyring) => Ok(Oo7Vault(keyring)),
            Err(oo7::Error::File(err))
                if matches!(
                    *err,
                    oo7::file::Error::Portal(oo7::ashpd::Error::PortalNotFound(_))
                ) =>
            {
                Err(OpenError::PortalNotFound(err.to_string()))
            }
            Err(err) => Err(OpenError::Other(err.to_string())),
        }
    }

    async fn host(&self) -> Result<Oo7Vault, String> {
        within(oo7::Keyring::host()).await.map(Oo7Vault)
    }
}

/// The production key storage.
pub struct Oo7KeyStore(Layered<Oo7Opener>);

impl Oo7KeyStore {
    #[must_use]
    pub fn new() -> Self {
        Self(Layered::new(Oo7Opener))
    }
}

impl Default for Oo7KeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Oo7KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oo7KeyStore")
            .field("backend", &self.0.backend())
            .finish()
    }
}

impl KeyStore for Oo7KeyStore {
    fn get<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>> {
        Box::pin(self.0.get(backup_id))
    }

    fn put<'a>(
        &'a self,
        backup_id: &'a str,
        backup_name: &'a str,
        key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(self.0.put(backup_id, backup_name, key))
    }

    fn delete<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(self.0.delete(backup_id))
    }

    fn backend(&self) -> Option<KeyBackend> {
        self.0.backend()
    }

    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>> {
        Box::pin(self.0.probe())
    }
}

/// Keys as files under the data folder, mode 0600. Not secure: dev mode
/// and tests only.
#[derive(Debug, Clone)]
pub struct PlainKeyStore {
    dir: PathBuf,
}

impl PlainKeyStore {
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file(&self, backup_id: &str) -> PathBuf {
        self.dir.join(format!("{backup_id}.key"))
    }
}

fn io_error(err: &std::io::Error) -> KeyError {
    KeyError::Unavailable(err.to_string())
}

impl KeyStore for PlainKeyStore {
    fn get<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>> {
        Box::pin(async move {
            match tokio::fs::read_to_string(self.file(backup_id)).await {
                Ok(text) => Key::parse(&text).map(Some),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(io_error(&err)),
            }
        })
    }

    fn put<'a>(
        &'a self,
        backup_id: &'a str,
        _backup_name: &'a str,
        key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(&self.dir)
                .await
                .map_err(|err| io_error(&err))?;
            let path = self.file(backup_id);
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .await
                .map_err(|err| io_error(&err))?;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|err| io_error(&err))?;
            file.write_all(key.password().as_bytes())
                .await
                .map_err(|err| io_error(&err))?;
            file.flush().await.map_err(|err| io_error(&err))
        })
    }

    fn delete<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(async move {
            match tokio::fs::remove_file(self.file(backup_id)).await {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(io_error(&err)),
            }
        })
    }

    fn backend(&self) -> Option<KeyBackend> {
        Some(KeyBackend::Plain)
    }

    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>> {
        Box::pin(async { Ok(KeyBackend::Plain) })
    }
}

/// Keys in the process, for tests of the code that needs a key storage.
#[derive(Debug, Default)]
pub struct MemoryKeyStore {
    keys: Mutex<HashMap<String, Key>>,
}

impl MemoryKeyStore {
    fn keys(&self) -> std::sync::MutexGuard<'_, HashMap<String, Key>> {
        self.keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl KeyStore for MemoryKeyStore {
    fn get<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>> {
        Box::pin(async move { Ok(self.keys().get(backup_id).cloned()) })
    }

    fn put<'a>(
        &'a self,
        backup_id: &'a str,
        _backup_name: &'a str,
        key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(async move {
            self.keys().insert(backup_id.to_owned(), key.clone());
            Ok(())
        })
    }

    fn delete<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), KeyError>> {
        Box::pin(async move {
            self.keys().remove(backup_id);
            Ok(())
        })
    }

    fn backend(&self) -> Option<KeyBackend> {
        Some(KeyBackend::Memory)
    }

    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>> {
        Box::pin(async { Ok(KeyBackend::Memory) })
    }
}

/// The key storage of this process: plain files when dev mode asks for
/// them, the keyring otherwise.
#[derive(Debug)]
pub enum DefaultKeyStore {
    Plain(PlainKeyStore),
    Keyring(Box<Oo7KeyStore>),
}

impl DefaultKeyStore {
    #[must_use]
    pub fn from_env(paths: &Paths) -> Self {
        let lookup = |key: &str| std::env::var_os(key);
        if plain_requested(&lookup) {
            if plain_allowed(&lookup) {
                return Self::Plain(PlainKeyStore::new(paths.dev_keys_dir()));
            }
            log::warn!(
                "{KEYSTORE_VAR}=plain is ignored: it needs {SYSTEMD_VAR}=off and no Flatpak sandbox"
            );
        }
        Self::Keyring(Box::default())
    }

    fn store(&self) -> &dyn KeyStore {
        match self {
            Self::Plain(store) => store,
            Self::Keyring(store) => store.as_ref(),
        }
    }
}

fn plain_requested(lookup: &impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    lookup(KEYSTORE_VAR).is_some_and(|value| value == "plain")
}

fn plain_allowed(lookup: &impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    lookup(SYSTEMD_VAR).is_some_and(|value| value == "off")
        && flatpak::app_id_from(lookup("FLATPAK_ID")).is_none()
}

impl KeyStore for DefaultKeyStore {
    fn get<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<Option<Key>, KeyError>> {
        self.store().get(backup_id)
    }

    fn put<'a>(
        &'a self,
        backup_id: &'a str,
        backup_name: &'a str,
        key: &'a Key,
    ) -> BoxFuture<'a, Result<(), KeyError>> {
        self.store().put(backup_id, backup_name, key)
    }

    fn delete<'a>(&'a self, backup_id: &'a str) -> BoxFuture<'a, Result<(), KeyError>> {
        self.store().delete(backup_id)
    }

    fn backend(&self) -> Option<KeyBackend> {
        self.store().backend()
    }

    fn probe(&self) -> BoxFuture<'_, Result<KeyBackend, KeyError>> {
        self.store().probe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Arc;

    const SAMPLE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLMNOPQRST";

    fn key() -> Key {
        Key::parse(SAMPLE).unwrap()
    }

    fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[derive(Clone, Default)]
    struct FakeVault {
        items: Arc<Mutex<HashMap<String, (String, String)>>>,
        broken: bool,
    }

    impl FakeVault {
        fn with(id: &str) -> Self {
            let vault = Self::default();
            vault
                .items
                .lock()
                .unwrap()
                .insert(id.to_owned(), (SAMPLE.to_owned(), "n".to_owned()));
            vault
        }

        fn holds(&self, id: &str) -> bool {
            self.items.lock().unwrap().contains_key(id)
        }
    }

    #[allow(clippy::unused_async_trait_impl, reason = "the trait is async")]
    impl Vault for FakeVault {
        async fn get(&self, id: &str) -> Result<Option<Key>, String> {
            if self.broken {
                return Err("vault broke".to_owned());
            }
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(id)
                .map(|(secret, _)| Key::parse(secret).unwrap()))
        }

        async fn put(&self, id: &str, name: &str, key: &Key) -> Result<(), String> {
            self.items
                .lock()
                .unwrap()
                .insert(id.to_owned(), (key.password().to_owned(), name.to_owned()));
            Ok(())
        }

        async fn delete(&self, id: &str) -> Result<(), String> {
            self.items.lock().unwrap().remove(id);
            Ok(())
        }
    }

    struct FakeOpener {
        portal: Result<FakeVault, String>,
        portal_missing: bool,
        host: Result<FakeVault, String>,
    }

    #[allow(clippy::unused_async_trait_impl, reason = "the trait is async")]
    impl Opener for FakeOpener {
        type Vault = FakeVault;

        async fn portal(&self) -> Result<FakeVault, OpenError> {
            match &self.portal {
                Ok(vault) => Ok(vault.clone()),
                Err(detail) if self.portal_missing => {
                    Err(OpenError::PortalNotFound(detail.clone()))
                }
                Err(detail) => Err(OpenError::Other(detail.clone())),
            }
        }

        async fn host(&self) -> Result<FakeVault, String> {
            self.host.clone()
        }
    }

    fn opener(
        portal: Result<FakeVault, (&str, bool)>,
        host: Result<FakeVault, &str>,
    ) -> Layered<FakeOpener> {
        let (portal, portal_missing) = match portal {
            Ok(vault) => (Ok(vault), false),
            Err((detail, missing)) => (Err(detail.to_owned()), missing),
        };
        Layered::new(FakeOpener {
            portal,
            portal_missing,
            host: host.map_err(str::to_owned),
        })
    }

    #[tokio::test]
    async fn the_portal_is_chosen_when_it_opens_even_if_the_host_does_too() {
        let store = opener(Ok(FakeVault::default()), Ok(FakeVault::default()));

        assert_eq!(store.backend(), None);
        assert_eq!(store.probe().await.unwrap(), KeyBackend::Portal);
        assert_eq!(store.backend(), Some(KeyBackend::Portal));
    }

    #[tokio::test]
    async fn a_missing_portal_falls_back_to_the_host_keyring() {
        let store = opener(Err(("no portal", true)), Ok(FakeVault::default()));

        assert_eq!(store.probe().await.unwrap(), KeyBackend::HostKeyring);
    }

    #[tokio::test]
    async fn any_other_portal_failure_does_not_fall_back_to_the_host() {
        let host = FakeVault::default();
        let store = opener(Err(("request denied", false)), Ok(host.clone()));

        let err = store.put("docs", "Docs", &key()).await.unwrap_err();

        assert_eq!(err, KeyError::Unavailable("request denied".to_owned()));
        assert!(!host.holds("docs"));
    }

    #[tokio::test]
    async fn without_a_portal_or_a_secret_service_the_storage_is_unavailable() {
        let store = opener(Err(("no portal", true)), Err("no bus"));

        let err = store.probe().await.unwrap_err();

        let KeyError::Unavailable(detail) = err else {
            panic!("expected Unavailable, got {err:?}");
        };
        assert!(
            detail.contains("no portal") && detail.contains("no bus"),
            "{detail}"
        );
    }

    #[tokio::test]
    async fn put_writes_only_to_the_chosen_backend() {
        let portal = FakeVault::default();
        let host = FakeVault::default();
        let store = opener(Ok(portal.clone()), Ok(host.clone()));

        store.put("docs", "Docs", &key()).await.unwrap();

        assert!(portal.holds("docs"));
        assert!(!host.holds("docs"));
    }

    #[tokio::test]
    async fn get_finds_a_key_the_other_backend_holds() {
        let host = FakeVault::with("docs");
        let store = opener(Ok(FakeVault::default()), Ok(host.clone()));

        assert_eq!(store.get("docs").await.unwrap(), Some(key()));
        assert!(host.holds("docs"));
    }

    #[tokio::test]
    async fn get_without_a_key_in_any_backend_is_none() {
        let store = opener(Ok(FakeVault::default()), Ok(FakeVault::default()));

        assert_eq!(store.get("docs").await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_prefers_the_chosen_backend() {
        let portal = FakeVault::default();
        portal
            .items
            .lock()
            .unwrap()
            .insert("docs".to_owned(), (SAMPLE.replace('A', "B"), String::new()));
        let store = opener(Ok(portal), Ok(FakeVault::with("docs")));

        let found = store.get("docs").await.unwrap().unwrap();

        assert_eq!(found.password(), SAMPLE.replace('A', "B"));
    }

    #[tokio::test]
    async fn a_failing_chosen_backend_is_an_error_not_a_miss() {
        let broken = FakeVault {
            broken: true,
            ..FakeVault::default()
        };
        let store = opener(Ok(broken), Ok(FakeVault::with("docs")));

        let err = store.get("docs").await.unwrap_err();

        assert_eq!(err, KeyError::Unavailable("vault broke".to_owned()));
    }

    #[tokio::test]
    async fn delete_removes_the_key_from_both_backends() {
        let portal = FakeVault::with("docs");
        let host = FakeVault::with("docs");
        let store = opener(Ok(portal.clone()), Ok(host.clone()));

        store.delete("docs").await.unwrap();

        assert!(!portal.holds("docs"));
        assert!(!host.holds("docs"));
    }

    #[tokio::test]
    async fn a_host_that_is_out_of_reach_does_not_stop_the_portal() {
        let portal = FakeVault::default();
        let store = opener(Ok(portal.clone()), Err("not allowed"));

        store.put("docs", "Docs", &key()).await.unwrap();

        assert_eq!(store.get("docs").await.unwrap(), Some(key()));
        store.delete("docs").await.unwrap();
        assert!(!portal.holds("docs"));
    }

    #[test]
    fn the_item_attributes_and_label_follow_the_spec() {
        assert_eq!(
            attributes("docs"),
            [
                (
                    "xdg:schema",
                    "io.github.raphael_soares.syncfast.BackupKey".to_owned()
                ),
                ("backup", "docs".to_owned()),
            ]
        );
        assert_eq!(label("Documents"), "syncfast backup key: Documents");
    }

    #[tokio::test]
    async fn the_plain_store_writes_the_key_with_mode_0600_and_reads_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = PlainKeyStore::new(dir.path().join("keys"));

        store.put("docs", "Docs", &key()).await.unwrap();

        let mode = std::fs::metadata(dir.path().join("keys/docs.key"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(store.get("docs").await.unwrap(), Some(key()));
        store.delete("docs").await.unwrap();
        assert_eq!(store.get("docs").await.unwrap(), None);
        store.delete("docs").await.unwrap();
    }

    #[tokio::test]
    async fn the_plain_store_tightens_a_file_that_was_wider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("docs.key");
        std::fs::write(&path, "x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let store = PlainKeyStore::new(dir.path().to_owned());

        store.put("docs", "Docs", &key()).await.unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SAMPLE);
    }

    #[tokio::test]
    async fn a_plain_file_that_is_not_a_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("docs.key"), "short").unwrap();
        let store = PlainKeyStore::new(dir.path().to_owned());

        assert_eq!(store.get("docs").await, Err(KeyError::Malformed));
    }

    #[tokio::test]
    async fn the_memory_store_keeps_replaces_and_forgets() {
        let store = MemoryKeyStore::default();
        assert_eq!(store.get("docs").await.unwrap(), None);

        store.put("docs", "Docs", &key()).await.unwrap();
        assert_eq!(store.get("docs").await.unwrap(), Some(key()));
        store.delete("docs").await.unwrap();

        assert_eq!(store.get("docs").await.unwrap(), None);
        assert_eq!(store.backend(), Some(KeyBackend::Memory));
        assert_eq!(store.probe().await.unwrap(), KeyBackend::Memory);
    }

    #[test]
    fn plain_needs_the_keystore_variable() {
        assert!(plain_requested(&lookup(&[("SYNCFAST_KEYSTORE", "plain")])));
        assert!(!plain_requested(&lookup(&[("SYNCFAST_KEYSTORE", "other")])));
        assert!(!plain_requested(&lookup(&[])));
    }

    #[test]
    fn plain_is_allowed_only_with_systemd_off_and_no_sandbox() {
        let dev = [("SYNCFAST_SYSTEMD", "off")];
        let sandboxed = [
            ("SYNCFAST_SYSTEMD", "off"),
            ("FLATPAK_ID", "io.example.App"),
        ];

        assert!(plain_allowed(&lookup(&dev)));
        assert!(!plain_allowed(&lookup(&sandboxed)));
        assert!(!plain_allowed(&lookup(&[])));
        assert!(!plain_allowed(&lookup(&[("SYNCFAST_SYSTEMD", "user")])));
    }
}
