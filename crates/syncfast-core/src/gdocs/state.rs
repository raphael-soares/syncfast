//! `gdocs.json`: the links a sync last wrote, by Drive file id.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::state::{StateError, write_json};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracked {
    pub links: BTreeMap<String, Link>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// The link file last written, relative to the sync's folder.
    pub path: String,
    /// The Drive name.
    pub name: String,
    /// The Drive folder, relative to the sync's remote.
    pub folder: String,
    /// For a shortcut, the mime type of its target.
    pub mime: String,
    pub url: String,
    /// Set when another file of the folder has the same name; never
    /// reassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u32>,
    #[serde(default)]
    pub shortcut: bool,
}

impl Tracked {
    /// A missing file is empty. One that is not valid JSON is set aside as
    /// `gdocs.json.corrupt-<stamp>` and counts as empty: every link is then
    /// written again, and nothing on Drive is touched for it.
    pub async fn load(paths: &Paths, pair_id: &str, stamp: &str) -> Self {
        let path = paths.gdocs_file(pair_id);
        match tokio::fs::read(&path).await {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(tracked) => tracked,
                Err(err) => {
                    set_aside(&path, stamp, &err).await;
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                log::warn!("cannot read {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub(crate) async fn exists(paths: &Paths, pair_id: &str) -> bool {
        tokio::fs::try_exists(paths.gdocs_file(pair_id))
            .await
            .unwrap_or(false)
    }

    /// # Errors
    /// The file cannot be written.
    pub async fn save(&self, paths: &Paths, pair_id: &str) -> Result<(), StateError> {
        write_json(paths.gdocs_file(pair_id), self).await
    }

    pub(crate) async fn remove(paths: &Paths, pair_id: &str) {
        let path = paths.gdocs_file(pair_id);
        if let Err(err) = tokio::fs::remove_file(&path).await
            && err.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("cannot delete {}: {err}", path.display());
        }
    }
}

async fn set_aside(path: &Path, stamp: &str, err: &serde_json::Error) {
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".corrupt-{stamp}"));
    log::warn!(
        "{} is not valid ({err}); keeping it as {}",
        path.display(),
        Path::new(&aside).display()
    );
    if let Err(err) = tokio::fs::rename(path, &aside).await {
        log::warn!("cannot set {} aside: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &Path) -> Paths {
        Paths::new(
            dir.join("h"),
            dir.join("c"),
            dir.join("s"),
            dir.join("d"),
            dir.join("r"),
        )
    }

    #[tokio::test]
    async fn a_corrupt_file_is_set_aside_and_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let file = paths.gdocs_file("docs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "{not json").unwrap();

        let tracked = Tracked::load(&paths, "docs", "20260102-150405").await;

        assert!(tracked.links.is_empty());
        assert!(!file.exists());
        assert!(
            file.with_file_name("gdocs.json.corrupt-20260102-150405")
                .exists()
        );
    }

    #[tokio::test]
    async fn saved_links_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let mut tracked = Tracked::default();
        tracked.links.insert(
            "id1".into(),
            Link {
                path: "a/Ata (2).gdoc.html".into(),
                name: "Ata".into(),
                folder: "a".into(),
                mime: "application/vnd.google-apps.document".into(),
                url: "https://x".into(),
                number: Some(2),
                shortcut: false,
            },
        );

        tracked.save(&paths, "docs").await.unwrap();

        assert_eq!(Tracked::load(&paths, "docs", "x").await, tracked);
    }
}
