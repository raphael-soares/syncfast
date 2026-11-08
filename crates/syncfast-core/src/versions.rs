//! Expiry of the local versions kept by `--backup-dir1`. Every round backs
//! up into `versions/<id>/<AAAAMMDD-HHMMSS>/`, so age is read from the name
//! of the dated folder and nothing else in `versions/<id>/` is touched.

use std::io;
use std::path::{Path, PathBuf};

use jiff::civil::DateTime;
use jiff::{SignedDuration, Zoned};

use crate::bisync::STAMP_FORMAT;
use crate::paths::Paths;

const STAMP_LEN: usize = 15;
const SECS_PER_DAY: i64 = 86_400;

fn parse_stamp(name: &str) -> Option<DateTime> {
    let bytes = name.as_bytes();
    let shaped = bytes.len() == STAMP_LEN
        && bytes[8] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, byte)| i == 8 || byte.is_ascii_digit());
    if !shaped {
        return None;
    }
    DateTime::strptime(STAMP_FORMAT, name).ok()
}

fn holds_no_files(dir: &Path) -> io::Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || !holds_no_files(&entry.path())? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn prune_entry(entry: &std::fs::DirEntry, cutoff: DateTime) -> io::Result<()> {
    let Some(stamp) = parse_stamp(&entry.file_name().to_string_lossy()) else {
        return Ok(());
    };
    if !entry.file_type()?.is_dir() {
        return Ok(());
    }
    let path: PathBuf = entry.path();
    if stamp < cutoff || holds_no_files(&path)? {
        std::fs::remove_dir_all(&path)?;
    }
    Ok(())
}

fn cutoff(keep_days: u32, now: &Zoned) -> io::Result<DateTime> {
    now.datetime()
        .checked_sub(SignedDuration::from_secs(
            i64::from(keep_days) * SECS_PER_DAY,
        ))
        .map_err(io::Error::other)
}

/// The names of `names` that are dated folders older than `keep_days`. Used
/// for versions that live on a remote, where age is also read from the name.
pub(crate) fn expired(names: &[String], keep_days: u32, now: &Zoned) -> io::Result<Vec<String>> {
    let cutoff = cutoff(keep_days, now)?;
    Ok(names
        .iter()
        .filter(|name| parse_stamp(name).is_some_and(|stamp| stamp < cutoff))
        .cloned()
        .collect())
}

/// How many dated folders could not be examined or deleted; each is
/// warned about and the rest are still pruned.
fn prune_blocking(root: &Path, keep_days: u32, now: &Zoned) -> io::Result<u32> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    let cutoff = cutoff(keep_days, now)?;
    let mut failures = 0;
    for entry in entries {
        let outcome = entry.and_then(|entry| {
            prune_entry(&entry, cutoff).map_err(|err| {
                io::Error::new(err.kind(), format!("{}: {err}", entry.path().display()))
            })
        });
        if let Err(err) = outcome {
            log::warn!("cannot prune old versions: {err}");
            failures += 1;
        }
    }
    Ok(failures)
}

/// Deletes the dated folders of `versions/<pair_id>/` older than
/// `keep_days`, and the ones with no file in them (rclone makes the folder
/// of a round even when nothing was backed up). A missing folder is fine.
/// Returns how many folders could not be pruned.
///
/// # Errors
/// The versions folder cannot be listed.
pub async fn prune(paths: &Paths, pair_id: &str, keep_days: u32, now: &Zoned) -> io::Result<u32> {
    let root = paths.versions_dir(pair_id);
    let now = now.clone();
    tokio::task::spawn_blocking(move || prune_blocking(&root, keep_days, &now))
        .await
        .unwrap_or_else(|join| Err(io::Error::other(join)))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use jiff::civil::date;
    use jiff::tz::TimeZone;

    use super::*;

    fn now() -> Zoned {
        date(2026, 3, 31)
            .at(12, 0, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
    }

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let paths = Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        );
        (dir, paths)
    }

    fn put(paths: &Paths, relative: &str) {
        let file = paths.versions_dir("p").join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "old").unwrap();
    }

    fn exists(paths: &Paths, relative: &str) -> bool {
        paths.versions_dir("p").join(relative).exists()
    }

    #[test]
    fn expired_names_are_the_dated_ones_older_than_the_keep_days() {
        let names: Vec<String> = [
            "20260228-115959",
            "20260301-120000",
            "notes",
            "20260101-000000x",
        ]
        .map(str::to_owned)
        .to_vec();

        let old = expired(&names, 30, &now()).unwrap();

        assert_eq!(old, ["20260228-115959"]);
    }

    #[tokio::test]
    async fn dated_folders_older_than_the_keep_days_are_deleted_with_their_content() {
        let (_dir, paths) = temp_paths();
        put(&paths, "20260228-115959/docs/a.txt");
        put(&paths, "20260301-120000/docs/b.txt");

        prune(&paths, "p", 30, &now()).await.unwrap();

        assert!(!exists(&paths, "20260228-115959"));
        assert!(exists(&paths, "20260301-120000/docs/b.txt"));
    }

    #[tokio::test]
    async fn everything_that_is_not_a_dated_folder_is_left_alone() {
        let (_dir, paths) = temp_paths();
        put(&paths, "notes-20200101-000000.txt");
        put(&paths, "20200101-000000-extra/a.txt");
        put(&paths, "2020010-1000000/a.txt");
        put(&paths, "kept-by-hand/a.txt");
        put(&paths, "20200101-000000");

        prune(&paths, "p", 30, &now()).await.unwrap();

        for name in [
            "notes-20200101-000000.txt",
            "20200101-000000-extra/a.txt",
            "2020010-1000000/a.txt",
            "kept-by-hand/a.txt",
        ] {
            assert!(exists(&paths, name), "{name}");
        }
        assert!(
            exists(&paths, "20200101-000000"),
            "a file is not a dated folder"
        );
    }

    #[tokio::test]
    async fn recent_dated_folders_with_no_files_are_removed() {
        let (_dir, paths) = temp_paths();
        std::fs::create_dir_all(paths.versions_dir("p").join("20260330-100000/sub")).unwrap();
        put(&paths, "20260330-110000/a.txt");

        prune(&paths, "p", 30, &now()).await.unwrap();

        assert!(!exists(&paths, "20260330-100000"));
        assert!(exists(&paths, "20260330-110000/a.txt"));
    }

    #[tokio::test]
    async fn a_missing_versions_folder_is_fine() {
        let (_dir, paths) = temp_paths();

        let result = prune(&paths, "p", 30, &now()).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn a_folder_that_cannot_be_deleted_is_counted_and_the_others_still_go() {
        let (_dir, paths) = temp_paths();
        put(&paths, "20200101-000000/locked/a.txt");
        put(&paths, "20200102-000000/a.txt");
        let locked = paths.versions_dir("p").join("20200101-000000/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        if std::fs::remove_file(locked.join("a.txt")).is_ok() {
            return; // permissions do not bind this user (root)
        }

        let failures = prune(&paths, "p", 30, &now()).await.unwrap();

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(failures, 1);
        assert!(!exists(&paths, "20200102-000000"));
        assert!(exists(&paths, "20200101-000000/locked/a.txt"));
    }

    #[tokio::test]
    async fn keep_days_come_from_the_argument() {
        let (_dir, paths) = temp_paths();
        put(&paths, "20260320-120000/a.txt");

        prune(&paths, "p", 10, &now()).await.unwrap();

        assert!(!exists(&paths, "20260320-120000"));
    }
}
