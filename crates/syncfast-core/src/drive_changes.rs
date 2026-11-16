//! Changes made on Drive by someone else (the web, another computer), found
//! through the Drive v3 changes API and turned into a wake of the affected
//! sync (ADR 0011). One poller per Drive account; the access token is read
//! from the rclone config and renewed by rclone, never written by syncfast.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config::split_remote;
use crate::drive_api::{DriveClient, DriveError, agent, get_blocking};
use crate::rclone::Rclone;
use crate::watch::Wake;

/// How often a poller asks Drive for changes.
pub const REMOTE_POLL: Duration = Duration::from_secs(30);
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
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

/// The address of the Google account `token` signs in to, as Drive reports
/// it. Blocks on the network.
///
/// # Errors
/// Drive cannot be reached, refuses the token or answers something else.
pub fn account_email(token: &str) -> Result<Option<String>, DriveError> {
    #[derive(Deserialize)]
    struct About {
        user: Option<User>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct User {
        email_address: Option<String>,
    }
    let about: About = get_blocking(
        &agent(),
        token,
        "/about",
        &[("fields", "user(emailAddress)".to_owned())],
    )?;
    Ok(about
        .user
        .and_then(|user| user.email_address)
        .filter(|email| !email.is_empty()))
}

/// What a poller keeps between two cycles.
struct Session {
    page_token: String,
    pairs: Vec<PairFolders>,
}

struct Poller {
    client: DriveClient,
    pairs: Vec<DrivePair>,
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
                        if wakes.send(Wake::Remote(id)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(err) => {
                    if !failing {
                        log::warn!("Drive changes of \"{}\": {err}", self.client.account());
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
        self.client.ensure_token().await?;
        let query = self.client.scope_query(false);
        let start: StartPageToken = self
            .client
            .get("/changes/startPageToken".to_owned(), query)
            .await?;
        let mut pairs = Vec::with_capacity(self.pairs.len());
        for pair in self.pairs.clone() {
            let folders: HashSet<String> = self
                .client
                .pair_folders(&pair.remote)
                .await?
                .into_iter()
                .map(|folder| folder.id)
                .collect();
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
        self.client.ensure_token().await?;
        let mut page = session.page_token.clone();
        let mut marked = BTreeSet::new();
        loop {
            let mut query = self.client.scope_query(true);
            query.extend([
                ("pageToken", page.clone()),
                ("pageSize", PAGE_SIZE.to_owned()),
                ("fields", CHANGE_FIELDS.to_owned()),
            ]);
            let answer: ChangesPage = self.client.get("/changes".to_owned(), query).await?;
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
                client: DriveClient::new(rclone.clone(), account),
                pairs,
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
}
