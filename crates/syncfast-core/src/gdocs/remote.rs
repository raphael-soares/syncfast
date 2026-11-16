//! The native Google files of a sync's folders, read from the Drive API.

use std::collections::HashMap;

use serde::Deserialize;

use super::link::{KINDS, kind_of_mime};
use crate::drive_api::{DriveClient, DriveError};
use crate::rclone::FolderId;

const SHORTCUT_MIME: &str = "application/vnd.google-apps.shortcut";
const FOLDERS_PER_QUERY: usize = 40;
pub(crate) const FILE_FIELDS: &str =
    "id,name,mimeType,parents,webViewLink,shortcutDetails(targetId,targetMimeType)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
    /// For a shortcut, the mime type of its target.
    pub mime: String,
    /// Relative to the sync's remote.
    pub folder: String,
    /// The id of `folder`.
    pub parent: String,
    pub url: String,
    pub shortcut: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DriveFile {
    id: String,
    name: String,
    mime_type: String,
    #[serde(default)]
    parents: Vec<String>,
    web_view_link: Option<String>,
    shortcut_details: Option<ShortcutDetails>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShortcutDetails {
    target_id: Option<String>,
    target_mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilesPage {
    next_page_token: Option<String>,
    #[serde(default)]
    files: Vec<DriveFile>,
}

/// What syncfast shows of `file`: a native file of a shown kind, or a
/// shortcut to one. The folder is the first parent `folders` knows.
pub(crate) fn shown(file: DriveFile, folders: &HashMap<&str, &str>) -> Option<RemoteFile> {
    let (parent, folder) = file
        .parents
        .iter()
        .find_map(|id| folders.get(id.as_str()).map(|path| (id.clone(), *path)))?;
    let (mime, url, shortcut) = if file.mime_type == SHORTCUT_MIME {
        let details = file.shortcut_details?;
        let target = details.target_id?;
        (
            details.target_mime_type?,
            format!("https://drive.google.com/open?id={target}"),
            true,
        )
    } else {
        (file.mime_type, file.web_view_link?, false)
    };
    kind_of_mime(&mime)?;
    Some(RemoteFile {
        id: file.id,
        name: file.name,
        mime,
        folder: folder.to_owned(),
        parent,
        url,
        shortcut,
    })
}

fn query(batch: &[FolderId]) -> String {
    let kinds = KINDS
        .iter()
        .map(|kind| kind.mime)
        .chain([SHORTCUT_MIME])
        .map(|mime| format!("mimeType='{mime}'"))
        .collect::<Vec<_>>()
        .join(" or ");
    let parents = batch
        .iter()
        .map(|folder| format!("'{}' in parents", folder.id))
        .collect::<Vec<_>>()
        .join(" or ");
    format!("trashed=false and ({kinds}) and ({parents})")
}

/// What the `files` endpoints need to reach a shared drive's files;
/// `listing` adds what only `files.list` takes.
pub(crate) fn files_scope(client: &DriveClient, listing: bool) -> Vec<(&'static str, String)> {
    let mut scope = vec![("supportsAllDrives", "true".to_owned())];
    if let Some(drive) = client.shared_drive().filter(|_| listing) {
        scope.extend([
            ("driveId", drive.to_owned()),
            ("corpora", "drive".to_owned()),
            ("includeItemsFromAllDrives", "true".to_owned()),
        ]);
    }
    scope
}

/// Every shown file directly inside one of `folders`.
///
/// # Errors
/// The token or Drive fails.
pub async fn list(
    client: &mut DriveClient,
    folders: &[FolderId],
) -> Result<Vec<RemoteFile>, DriveError> {
    client.ensure_token().await?;
    let paths: HashMap<&str, &str> = folders
        .iter()
        .map(|folder| (folder.id.as_str(), folder.path.as_str()))
        .collect();
    let mut found = Vec::new();
    for batch in folders.chunks(FOLDERS_PER_QUERY) {
        let mut page: Option<String> = None;
        loop {
            let mut params = files_scope(client, true);
            params.extend([
                ("q", query(batch)),
                ("fields", format!("nextPageToken,files({FILE_FIELDS})")),
                ("pageSize", "1000".to_owned()),
            ]);
            if let Some(token) = page.take() {
                params.push(("pageToken", token));
            }
            let answer: FilesPage = client.get("/files".to_owned(), params).await?;
            found.extend(
                answer
                    .files
                    .into_iter()
                    .filter_map(|file| shown(file, &paths)),
            );
            match answer.next_page_token {
                Some(next) => page = Some(next),
                None => break,
            }
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found.dedup_by(|a, b| a.id == b.id);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(json: &str) -> DriveFile {
        serde_json::from_str(json).unwrap()
    }

    fn folders() -> HashMap<&'static str, &'static str> {
        HashMap::from([("root", ""), ("fa", "a")])
    }

    #[test]
    fn a_doc_is_shown_with_its_web_link_and_folder() {
        let doc = file(
            r#"{"id":"d1","name":"Ata","mimeType":"application/vnd.google-apps.document",
                "parents":["fa"],"webViewLink":"https://docs.google.com/document/d/d1/edit"}"#,
        );

        let shown = shown(doc, &folders()).unwrap();

        assert_eq!(
            (shown.folder.as_str(), shown.url.as_str(), shown.shortcut),
            ("a", "https://docs.google.com/document/d/d1/edit", false)
        );
    }

    #[test]
    fn a_shortcut_to_a_sheet_opens_its_target() {
        let shortcut = file(
            r#"{"id":"s1","name":"Budget","mimeType":"application/vnd.google-apps.shortcut",
                "parents":["root"],"shortcutDetails":{"targetId":"t9",
                "targetMimeType":"application/vnd.google-apps.spreadsheet"}}"#,
        );

        let shown = shown(shortcut, &folders()).unwrap();

        assert_eq!(shown.id, "s1");
        assert_eq!(shown.url, "https://drive.google.com/open?id=t9");
        assert_eq!(shown.mime, "application/vnd.google-apps.spreadsheet");
        assert!(shown.shortcut);
    }

    #[test]
    fn a_shortcut_to_a_plain_file_and_an_apps_script_are_not_shown() {
        let to_pdf = file(
            r#"{"id":"s2","name":"x","mimeType":"application/vnd.google-apps.shortcut",
                "parents":["root"],"shortcutDetails":{"targetId":"t1",
                "targetMimeType":"application/pdf"}}"#,
        );
        let script = file(
            r#"{"id":"g1","name":"x","mimeType":"application/vnd.google-apps.script",
                "parents":["root"],"webViewLink":"https://x"}"#,
        );

        assert!(shown(to_pdf, &folders()).is_none());
        assert!(shown(script, &folders()).is_none());
    }
}
