//! The email address of each account, read from Google after a sign-in, so
//! the screens can name an account by the address its owner knows.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use crate::paths::Paths;

fn file(paths: &Paths) -> PathBuf {
    paths.state_dir().join("accounts.json")
}

/// Account name to email. A missing or unreadable file is an empty map.
#[must_use]
pub fn load(paths: &Paths) -> BTreeMap<String, String> {
    std::fs::read(file(paths))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// # Errors
/// The file cannot be written.
pub fn save(paths: &Paths, name: &str, email: &str) -> io::Result<()> {
    let mut labels = load(paths);
    labels.insert(name.to_owned(), email.to_owned());
    write(paths, &labels)
}

/// # Errors
/// The file cannot be written.
pub fn forget(paths: &Paths, name: &str) -> io::Result<()> {
    let mut labels = load(paths);
    if labels.remove(name).is_none() {
        return Ok(());
    }
    write(paths, &labels)
}

fn write(paths: &Paths, labels: &BTreeMap<String, String>) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(labels).map_err(io::Error::other)?;
    fastframe_file::write_private(&file(paths), json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths_in(root: &std::path::Path) -> Paths {
        Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        )
    }

    #[test]
    fn a_saved_email_is_loaded_back_by_account_name() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());

        save(&paths, "drive", "ada@example.com").unwrap();
        save(&paths, "drive-2", "bob@example.com").unwrap();

        assert_eq!(
            load(&paths),
            BTreeMap::from([
                ("drive".to_owned(), "ada@example.com".to_owned()),
                ("drive-2".to_owned(), "bob@example.com".to_owned()),
            ])
        );
    }

    #[test]
    fn a_forgotten_account_loses_only_its_own_email() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        save(&paths, "drive", "ada@example.com").unwrap();
        save(&paths, "work", "bob@example.com").unwrap();

        forget(&paths, "drive").unwrap();

        assert_eq!(
            load(&paths),
            BTreeMap::from([("work".to_owned(), "bob@example.com".to_owned())])
        );
    }

    #[test]
    fn a_missing_or_broken_file_reads_as_no_emails() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths_in(dir.path());
        assert!(load(&paths).is_empty());

        std::fs::create_dir_all(paths.state_dir()).unwrap();
        std::fs::write(paths.state_dir().join("accounts.json"), "not json").unwrap();

        assert!(load(&paths).is_empty());
    }
}
