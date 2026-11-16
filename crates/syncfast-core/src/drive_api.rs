//! A small Drive v3 client. The access token is read from the rclone config
//! and renewed by rclone, never written by syncfast, never logged.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::config::split_remote;
use crate::rclone::{DriveToken, FolderId, Rclone, RcloneError};

pub(crate) const API: &str = "https://www.googleapis.com/drive/v3";
/// A token this close to its expiry is renewed before it is used.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum DriveError {
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
    pub(crate) fn resets_session(&self) -> bool {
        matches!(
            self,
            Self::Http {
                status: 400 | 404 | 410,
                ..
            }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Patch,
    Post,
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

pub(crate) fn agent() -> ureq::Agent {
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

fn read_answer<T: DeserializeOwned>(
    answer: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<T, DriveError> {
    let mut response = answer.map_err(|err| DriveError::Transport(err.to_string()))?;
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

pub(crate) fn get_blocking<T: DeserializeOwned>(
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
    read_answer(request.call())
}

fn send_blocking<T: DeserializeOwned>(
    agent: &ureq::Agent,
    token: &str,
    method: Method,
    path: &str,
    query: &[(&str, String)],
    body: &serde_json::Value,
) -> Result<T, DriveError> {
    let url = format!("{API}{path}");
    let mut request = match method {
        Method::Patch => agent.patch(url),
        Method::Post => agent.post(url),
    }
    .header("Authorization", format!("Bearer {token}"));
    for (key, value) in query {
        request = request.query(key, value);
    }
    read_answer(request.send_json(body))
}

#[derive(Debug, Deserialize)]
struct FileId {
    id: String,
}

/// One Drive account, reached with the token rclone saved for it.
pub struct DriveClient {
    rclone: Rclone,
    agent: ureq::Agent,
    account: String,
    token: Option<DriveToken>,
}

impl std::fmt::Debug for DriveClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriveClient")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl DriveClient {
    /// `account` is the rclone remote name, without the colon.
    #[must_use]
    pub fn new(rclone: Rclone, account: String) -> Self {
        Self {
            rclone,
            agent: agent(),
            account,
            token: None,
        }
    }

    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    /// The shared drive the account is scoped to, once a token was read.
    #[must_use]
    pub fn shared_drive(&self) -> Option<&str> {
        self.token
            .as_ref()
            .and_then(|token| token.team_drive.as_deref())
    }

    /// The shared drive scope, when the account is one. `listing` adds what
    /// only a list call takes.
    #[must_use]
    pub fn scope_query(&self, listing: bool) -> Vec<(&'static str, String)> {
        match (self.shared_drive().map(str::to_owned), listing) {
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

    /// The folders of the sync whose remote is `remote` (`account:folder`):
    /// its root, with an empty path, then every folder under it.
    ///
    /// # Errors
    /// The token, rclone or Drive fails, or the folder is not on Drive.
    pub async fn pair_folders(&mut self, remote: &str) -> Result<Vec<FolderId>, DriveError> {
        let folder = split_remote(remote).map_or("", |(_, folder)| folder.trim_matches('/'));
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
        let mut folders = vec![FolderId {
            path: String::new(),
            id: root,
        }];
        folders.extend(
            self.rclone
                .list_folder_ids(&format!("{}:{folder}", self.account), true)
                .await?,
        );
        Ok(folders)
    }

    /// The id of the folder the account treats as its root.
    ///
    /// # Errors
    /// The token or Drive fails.
    pub async fn account_root(&mut self) -> Result<String, DriveError> {
        self.ensure_token().await?;
        let token = self.token.as_ref().ok_or(DriveError::NoToken)?;
        if let Some(id) = token.root_folder_id.clone().or(token.team_drive.clone()) {
            return Ok(id);
        }
        let root: FileId = self
            .get("/files/root".to_owned(), vec![("fields", "id".to_owned())])
            .await?;
        Ok(root.id)
    }

    /// The saved token, renewed first when it is about to expire.
    ///
    /// # Errors
    /// rclone fails or holds no token for the account.
    pub async fn ensure_token(&mut self) -> Result<(), DriveError> {
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

    /// `GET {API}{path}`, renewing the token once when Drive refuses it.
    ///
    /// # Errors
    /// The token, the network or Drive fails, or the answer is not a `T`.
    pub async fn get<T: DeserializeOwned + Send + 'static>(
        &mut self,
        path: String,
        query: Vec<(&'static str, String)>,
    ) -> Result<T, DriveError> {
        self.ensure_token().await?;
        match self.call(None, path.clone(), query.clone()).await {
            Err(DriveError::Http { status: 401, .. }) => {
                self.token = Some(self.renew().await?);
                self.call(None, path, query).await
            }
            other => other,
        }
    }

    /// `PATCH` or `POST` of a JSON body, renewing the token once when Drive
    /// refuses it.
    ///
    /// # Errors
    /// The token, the network or Drive fails, or the answer is not a `T`.
    pub async fn send_json<T: DeserializeOwned + Send + 'static>(
        &mut self,
        method: Method,
        path: String,
        query: Vec<(&'static str, String)>,
        body: serde_json::Value,
    ) -> Result<T, DriveError> {
        self.ensure_token().await?;
        let send = Some((method, body));
        match self.call(send.clone(), path.clone(), query.clone()).await {
            Err(DriveError::Http { status: 401, .. }) => {
                self.token = Some(self.renew().await?);
                self.call(send, path, query).await
            }
            other => other,
        }
    }

    async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        send: Option<(Method, serde_json::Value)>,
        path: String,
        query: Vec<(&'static str, String)>,
    ) -> Result<T, DriveError> {
        let token = self.token.as_ref().ok_or(DriveError::NoToken)?;
        let token = Zeroizing::new(token.access_token.as_str().to_owned());
        let agent = self.agent.clone();
        tokio::task::spawn_blocking(move || match send {
            None => get_blocking(&agent, &token, &path, &query),
            Some((method, body)) => send_blocking(&agent, &token, method, &path, &query, &body),
        })
        .await
        .map_err(|err| DriveError::Transport(err.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
