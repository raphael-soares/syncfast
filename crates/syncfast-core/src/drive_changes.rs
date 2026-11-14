//! Changes made on Drive by someone else (the web, another computer), found
//! through the Drive v3 changes API and turned into a wake of the affected
//! sync (ADR 0011). One poller per Drive account; the access token is read
//! from the rclone config and renewed by rclone, never written by syncfast.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zeroize::Zeroizing;

use crate::config::split_remote;
use crate::rclone::{DriveToken, Rclone, RcloneError};
use crate::watch::Wake;

/// How often a poller asks Drive for changes.
pub const REMOTE_POLL: Duration = Duration::from_secs(30);
const API: &str = "https://www.googleapis.com/drive/v3";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// A token this close to its expiry is renewed before it is used.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_SIZE: &str = "1000";
const CHANGE_FIELDS: &str =
    "nextPageToken,newStartPageToken,changes(fileId,removed,file(mimeType,parents))";

/// A sync that reads from Drive: its id and `<account>:<folder>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrivePair {
    pub id: String,
    pub remote: String,
}

/// The ids of the folders that make up a sync's remote tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairFolders {
    pub id: String,
    pub folders: HashSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesPage {
    #[serde(default)]
    pub changes: Vec<Change>,
    pub next_page_token: Option<String>,
    pub new_start_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub file_id: Option<String>,
    #[serde(default)]
    pub removed: bool,
    pub file: Option<ChangedFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangedFile {
    pub mime_type: Option<String>,
    #[serde(default)]
    pub parents: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartPageToken {
    start_page_token: String,
}

#[derive(Debug, Deserialize)]
struct FileId {
    id: String,
}

/// The syncs `changes` touch, in the order of `pairs`. A change is inside a
/// sync when its file or one of its parents is a folder of that sync; a new
/// folder inside a sync joins the sync's set, so what is created in it later
/// is seen too. A removal carries no file and so no parent: it marks every
/// sync of the account.
#[must_use]
pub fn marked_pairs(changes: &[Change], pairs: &mut [PairFolders]) -> Vec<String> {
    let mut marked = vec![false; pairs.len()];
    for change in changes {
        let Some(file) = &change.file else {
            if change.removed {
                marked.fill(true);
            }
            continue;
        };
        let id = change.file_id.as_deref();
        let is_folder = file.mime_type.as_deref() == Some(FOLDER_MIME);
        for (pair, marked) in pairs.iter_mut().zip(&mut marked) {
            let inside = file
                .parents
                .iter()
                .any(|parent| pair.folders.contains(parent));
            let known = id.is_some_and(|id| pair.folders.contains(id));
            if !inside && !known {
                continue;
            }
            *marked = true;
            if inside
                && is_folder
                && let Some(id) = id
            {
                pair.folders.insert(id.to_owned());
            }
        }
    }
    pairs
        .iter()
        .zip(marked)
        .filter(|(_, marked)| *marked)
        .map(|(pair, _)| pair.id.clone())
        .collect()
}

#[derive(Debug, thiserror::Error)]
enum DriveError {
    #[error("Drive answered {status}: {message}")]
    Http { status: u16, message: String },
    #[error("cannot reach Drive: {0}")]
    Transport(String),
    #[error("{0}")]
    Rclone(#[from] RcloneError),
    #[error("no sign-in saved for the account")]
    NoToken,
    #[error("the folder \"{0}\" is not on Drive")]
    FolderMissing(String),
    #[error("unexpected answer from Drive")]
    BadAnswer,
}

impl DriveError {
    /// The page token or the folder ids can no longer be trusted.
    fn resets_session(&self) -> bool {
        matches!(
            self,
            Self::Http {
                status: 400 | 404 | 410,
                ..
            }
        )
    }
}

fn google_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct Envelope {
        error: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        message: String,
    }
    serde_json::from_str::<Envelope>(body).map_or_else(
        |_| "no message".to_owned(),
        |envelope| envelope.error.message,
    )
}

fn agent() -> ureq::Agent {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::Rustls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(provider)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

fn get_blocking<T: DeserializeOwned>(
    agent: &ureq::Agent,
    token: &str,
    path: &str,
    query: &[(&str, String)],
) -> Result<T, DriveError> {
    let mut request = agent
        .get(format!("{API}{path}"))
        .header("Authorization", format!("Bearer {token}"));
    for (key, value) in query {
        request = request.query(key, value);
    }
    let mut response = request
        .call()
        .map_err(|err| DriveError::Transport(err.to_string()))?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return response
            .body_mut()
            .read_json()
            .map_err(|_| DriveError::BadAnswer);
    }
    let body = response.body_mut().read_to_string().unwrap_or_default();
    Err(DriveError::Http {
        status,
        message: google_message(&body),
    })
}

/// What a poller keeps between two cycles.
struct Session {
    page_token: String,
    pairs: Vec<PairFolders>,
}

struct Poller {
    rclone: Rclone,
    agent: ureq::Agent,
    account: String,
    pairs: Vec<DrivePair>,
    token: Option<DriveToken>,
    session: Option<Session>,
}

impl Poller {
    async fn run(mut self, wakes: mpsc::Sender<Wake>) {
        let mut failing = false;
        loop {
            match self.cycle().await {
                Ok(marked) => {
                    failing = false;
                    for id in marked {
                        log::debug!("Drive changed under \"{id}\"");
                        if wakes.send(Wake::Remote(id)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(err) => {
                    if !failing {
                        log::warn!("Drive changes of \"{}\": {err}", self.account);
                    }
                    failing = true;
                    if err.resets_session() {
                        self.session = None;
                    }
                }
            }
            tokio::time::sleep(REMOTE_POLL).await;
        }
    }

    async fn cycle(&mut self) -> Result<Vec<String>, DriveError> {
        let Some(mut session) = self.session.take() else {
            self.session = Some(self.begin().await?);
            return Ok(Vec::new());
        };
        let result = self.poll(&mut session).await;
        self.session = Some(session);
        result
    }

    async fn begin(&mut self) -> Result<Session, DriveError> {
        self.ensure_token().await?;
        let start: StartPageToken = self
            .get("/changes/startPageToken", self.scope_query(false))
            .await?;
        let mut pairs = Vec::with_capacity(self.pairs.len());
        for pair in self.pairs.clone() {
            let folders = self.folders_of(&pair).await?;
            pairs.push(PairFolders {
                id: pair.id,
                folders,
            });
        }
        Ok(Session {
            page_token: start.start_page_token,
            pairs,
        })
    }

    async fn poll(&mut self, session: &mut Session) -> Result<Vec<String>, DriveError> {
        self.ensure_token().await?;
        let mut page = session.page_token.clone();
        let mut marked = BTreeSet::new();
        loop {
            let mut query = self.scope_query(true);
            query.extend([
                ("pageToken", page.clone()),
                ("pageSize", PAGE_SIZE.to_owned()),
                ("fields", CHANGE_FIELDS.to_owned()),
            ]);
            let answer: ChangesPage = self.get("/changes", query).await?;
            marked.extend(marked_pairs(&answer.changes, &mut session.pairs));
            match (answer.next_page_token, answer.new_start_page_token) {
                (Some(next), _) => page = next,
                (None, Some(start)) => {
                    session.page_token = start;
                    return Ok(marked.into_iter().collect());
                }
                (None, None) => return Err(DriveError::BadAnswer),
            }
        }
    }

    /// The shared drive scope, when the account is one. `listing` adds what
    /// only the change list takes.
    fn scope_query(&self, listing: bool) -> Vec<(&'static str, String)> {
        let shared = self
            .token
            .as_ref()
            .and_then(|token| token.team_drive.clone());
        match (shared, listing) {
            (Some(drive), true) => vec![
                ("driveId", drive),
                ("supportsAllDrives", "true".to_owned()),
                ("includeItemsFromAllDrives", "true".to_owned()),
            ],
            (Some(drive), false) => {
                vec![("driveId", drive), ("supportsAllDrives", "true".to_owned())]
            }
            (None, true) => vec![("restrictToMyDrive", "true".to_owned())],
            (None, false) => Vec::new(),
        }
    }

    async fn folders_of(&mut self, pair: &DrivePair) -> Result<HashSet<String>, DriveError> {
        let folder = split_remote(&pair.remote).map_or("", |(_, folder)| folder.trim_matches('/'));
        let root = if folder.is_empty() {
            self.account_root().await?
        } else {
            let (parent, leaf) = folder.rsplit_once('/').unwrap_or(("", folder));
            self.rclone
                .list_folder_ids(&format!("{}:{parent}", self.account), false)
                .await?
                .into_iter()
                .find(|entry| entry.path == leaf)
                .map(|entry| entry.id)
                .ok_or_else(|| DriveError::FolderMissing(folder.to_owned()))?
        };
        let mut folders: HashSet<String> = self
            .rclone
            .list_folder_ids(&format!("{}:{folder}", self.account), true)
            .await?
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        folders.insert(root);
        Ok(folders)
    }

    async fn account_root(&mut self) -> Result<String, DriveError> {
        self.ensure_token().await?;
        let token = self.token.as_ref().ok_or(DriveError::NoToken)?;
        if let Some(id) = token.root_folder_id.clone().or(token.team_drive.clone()) {
            return Ok(id);
        }
        let root: FileId = self
            .get("/files/root", vec![("fields", "id".to_owned())])
            .await?;
        Ok(root.id)
    }

    /// The saved token, renewed first when it is about to expire.
    async fn ensure_token(&mut self) -> Result<(), DriveError> {
        let current = match self.token.take() {
            Some(token) => token,
            None => self.read_token().await?,
        };
        let token = if current.expires_within(jiff::Timestamp::now(), TOKEN_MARGIN) {
            self.renew().await?
        } else {
            current
        };
        self.token = Some(token);
        Ok(())
    }

    async fn read_token(&self) -> Result<DriveToken, DriveError> {
        self.rclone
            .drive_token(&self.account)
            .await?
            .ok_or(DriveError::NoToken)
    }

    /// `rclone about` makes rclone renew the token and save it in its own
    /// config; reading it again gives the new one.
    async fn renew(&self) -> Result<DriveToken, DriveError> {
        self.rclone.check_remote(&self.account).await?;
        self.read_token().await
    }

    async fn get<T: DeserializeOwned + Send + 'static>(
        &mut self,
        path: &'static str,
        query: Vec<(&'static str, String)>,
    ) -> Result<T, DriveError> {
        self.ensure_token().await?;
        match self.call(path, query.clone()).await {
            Err(DriveError::Http { status: 401, .. }) => {
                self.token = Some(self.renew().await?);
                self.call(path, query).await
            }
            other => other,
        }
    }

    async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &'static str,
        query: Vec<(&'static str, String)>,
    ) -> Result<T, DriveError> {
        let token = self.token.as_ref().ok_or(DriveError::NoToken)?;
        let token = Zeroizing::new(token.access_token.as_str().to_owned());
        let agent = self.agent.clone();
        tokio::task::spawn_blocking(move || get_blocking(&agent, &token, path, &query))
            .await
            .map_err(|err| DriveError::Transport(err.to_string()))?
    }
}

/// Starts one poller per account for `pairs`. They end when the returned
/// handles are aborted or `wakes` has no receiver.
#[must_use]
pub fn spawn_pollers(
    rclone: &Rclone,
    pairs: Vec<DrivePair>,
    wakes: &mpsc::Sender<Wake>,
) -> Vec<JoinHandle<()>> {
    by_account(pairs)
        .into_iter()
        .map(|(account, pairs)| {
            let poller = Poller {
                rclone: rclone.clone(),
                agent: agent(),
                account,
                pairs,
                token: None,
                session: None,
            };
            tokio::spawn(poller.run(wakes.clone()))
        })
        .collect()
}

fn by_account(pairs: Vec<DrivePair>) -> Vec<(String, Vec<DrivePair>)> {
    let mut accounts: Vec<(String, Vec<DrivePair>)> = Vec::new();
    for pair in pairs {
        let Some((account, _)) = split_remote(&pair.remote) else {
            continue;
        };
        match accounts.iter_mut().find(|(name, _)| name == account) {
            Some((_, group)) => group.push(pair),
            None => accounts.push((account.to_owned(), vec![pair])),
        }
    }
    accounts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn pairs() -> Vec<PairFolders> {
        vec![
            PairFolders {
                id: "udesc".to_owned(),
                folders: set(&["udesc-root", "udesc-provas"]),
            },
            PairFolders {
                id: "notes".to_owned(),
                folders: set(&["notes-root"]),
            },
        ]
    }

    fn page(json: &str) -> ChangesPage {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_change_inside_one_sync_marks_only_that_sync() {
        let answer = page(
            r#"{
              "kind": "drive#changeList",
              "newStartPageToken": "812",
              "changes": [
                {"kind": "drive#change", "changeType": "file", "time": "2026-11-13T10:00:00.000Z",
                 "fileId": "f1", "removed": false,
                 "file": {"kind": "drive#file", "mimeType": "application/pdf", "parents": ["udesc-provas"]}},
                {"kind": "drive#change", "changeType": "file", "fileId": "elsewhere", "removed": false,
                 "file": {"mimeType": "text/plain", "parents": ["somebody-elses-folder"]}}
              ]
            }"#,
        );
        let mut pairs = pairs();

        let marked = marked_pairs(&answer.changes, &mut pairs);

        assert_eq!(marked, ["udesc"]);
        assert_eq!(answer.new_start_page_token.as_deref(), Some("812"));
        assert_eq!(answer.next_page_token, None);
    }

    #[test]
    fn a_change_to_a_known_folder_itself_marks_its_sync() {
        let answer = page(
            r#"{"changes": [{"fileId": "notes-root", "removed": false,
                "file": {"mimeType": "application/vnd.google-apps.folder", "parents": ["my-drive"]}}]}"#,
        );
        let mut pairs = pairs();

        assert_eq!(marked_pairs(&answer.changes, &mut pairs), ["notes"]);
    }

    #[test]
    fn a_new_folder_joins_the_set_so_what_is_created_in_it_later_is_seen() {
        let created = page(
            r#"{"nextPageToken": "next", "changes": [{"fileId": "new-dir", "removed": false,
                "file": {"mimeType": "application/vnd.google-apps.folder", "parents": ["udesc-root"]}}]}"#,
        );
        let inside = page(
            r#"{"newStartPageToken": "9", "changes": [{"fileId": "doc", "removed": false,
                "file": {"mimeType": "text/plain", "parents": ["new-dir"]}}]}"#,
        );
        let mut pairs = pairs();

        let first = marked_pairs(&created.changes, &mut pairs);
        let second = marked_pairs(&inside.changes, &mut pairs);

        assert_eq!(first, ["udesc"]);
        assert!(pairs[0].folders.contains("new-dir"));
        assert_eq!(second, ["udesc"]);
        assert_eq!(created.next_page_token.as_deref(), Some("next"));
    }

    #[test]
    fn a_new_file_does_not_join_the_set() {
        let answer = page(
            r#"{"changes": [{"fileId": "doc", "removed": false,
                "file": {"mimeType": "text/plain", "parents": ["udesc-root"]}}]}"#,
        );
        let mut pairs = pairs();

        let _ = marked_pairs(&answer.changes, &mut pairs);

        assert!(!pairs[0].folders.contains("doc"));
    }

    #[test]
    fn a_removal_without_a_file_marks_every_sync_of_the_account() {
        let answer = page(r#"{"changes": [{"fileId": "gone", "removed": true}]}"#);
        let mut pairs = pairs();

        let marked = marked_pairs(&answer.changes, &mut pairs);

        assert_eq!(marked, ["udesc", "notes"]);
    }

    #[test]
    fn no_changes_mark_nothing() {
        let answer = page(r#"{"newStartPageToken": "3", "changes": []}"#);
        let mut pairs = pairs();

        assert!(marked_pairs(&answer.changes, &mut pairs).is_empty());
    }

    #[test]
    fn syncs_are_grouped_by_account_in_the_order_they_appear() {
        let pair = |id: &str, remote: &str| DrivePair {
            id: id.to_owned(),
            remote: remote.to_owned(),
        };

        let groups = by_account(vec![
            pair("a", "one:A"),
            pair("b", "two:"),
            pair("c", "one:C/deep"),
        ]);

        assert_eq!(
            groups,
            [
                (
                    "one".to_owned(),
                    vec![pair("a", "one:A"), pair("c", "one:C/deep")]
                ),
                ("two".to_owned(), vec![pair("b", "two:")])
            ]
        );
    }

    #[test]
    fn the_message_of_a_google_error_is_kept_and_anything_else_is_not_echoed() {
        let body = r#"{"error": {"code": 403, "message": "Rate Limit Exceeded", "errors": []}}"#;

        assert_eq!(google_message(body), "Rate Limit Exceeded");
        assert_eq!(google_message("<html>secret</html>"), "no message");
    }

    #[test]
    fn only_a_bad_page_token_or_a_missing_resource_resets_the_session() {
        let http = |status| DriveError::Http {
            status,
            message: String::new(),
        };

        assert!(http(410).resets_session());
        assert!(http(400).resets_session());
        assert!(!http(500).resets_session());
        assert!(!http(403).resets_session());
        assert!(!DriveError::Transport("down".to_owned()).resets_session());
    }
}
