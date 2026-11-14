//! What a sync deleted or replaced that can still come back: the local
//! versions (`versions/<id>/<AAAAMMDD-HHMMSS>/`), the versions a `mirror_up`
//! keeps on the account (`.syncfast-versions/<id>/`) and, when the account
//! is a Google Drive, the Drive trash. Restoring always writes into the
//! sync's local folder, never over a file, and the next round carries it
//! across.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp, Zoned};
use serde::{Deserialize, Serialize};

use crate::behavior::{Behavior, SyncMode};
use crate::config::{Pair, split_remote};
use crate::conflicts::{copy_without_overwriting, occupied, split_extension};
use crate::oneway::{REMOTE_VERSIONS_DIR, remote_versions_dir};
use crate::paths::Paths;
use crate::rclone::{DRIVE_TYPE, Rclone, RcloneError};
use crate::versions::parse_stamp;

const SECS_PER_DAY: i64 = 86_400;
const RESTORED_FORMAT: &str = "%Y-%m-%d %H-%M";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletedSide {
    /// Kept in the versions folder of this sync, on this computer.
    Local,
    /// Kept by `mirror_up` in `.syncfast-versions/<id>/` on the account.
    Remote,
    /// In the Drive trash of the account.
    Drive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletedKind {
    /// Nothing is at the path any more.
    Deleted,
    /// A newer file is at the path; this is the old content.
    Replaced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeletedItem {
    pub side: DeletedSide,
    pub kind: DeletedKind,
    /// Relative to the sync's folder, with `/` between the parts.
    pub path: String,
    /// When the round that kept it ran. `None` in the Drive trash.
    pub when: Option<Timestamp>,
    /// When the versions folder lets it go. `None` in the Drive trash.
    pub kept_until: Option<Timestamp>,
    pub size: u64,
    /// What [`restore`] takes: `<stamp>/<path>` for a version, the file id
    /// for the Drive trash.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeletedList {
    pub items: Vec<DeletedItem>,
    /// What is missing from `items` because the account could not be read.
    pub warning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeletedError {
    #[error("nothing to restore under {0}")]
    NotFound(String),
    #[error(transparent)]
    Rclone(#[from] RcloneError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Versions newest first, then the Drive trash by path. What cannot be read
/// on the account is left out with a warning, and the local versions stay.
///
/// # Errors
/// The local versions folder cannot be read.
pub async fn list(
    paths: &Paths,
    rclone: &Rclone,
    pair: &Pair,
    behavior: &Behavior,
) -> Result<DeletedList, DeletedError> {
    let root = paths.versions_dir(&pair.id);
    let local = pair.local.clone();
    let keep = keep_for(behavior.versions_keep_days);
    let mut items = tokio::task::spawn_blocking(move || local_items(&root, &local, keep))
        .await
        .map_err(io::Error::other)??;
    let mut warnings = Vec::new();
    if behavior.mode == SyncMode::MirrorUp {
        match remote_items(rclone, pair, keep).await {
            Ok(remote) => items.extend(remote),
            Err(err) => warnings.push(format!("cannot read the versions on the account: {err}")),
        }
    }
    sort_newest_first(&mut items);
    match drive_items(rclone, pair).await {
        Ok(drive) => items.extend(drive),
        Err(err) => warnings.push(format!("cannot read the Drive trash: {err}")),
    }
    Ok(DeletedList {
        items,
        warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
    })
}

/// Copies the item `key` of `side` back into the sync's folder. Where a
/// file is already there, it becomes `<name> (restored <date time>).<ext>`.
/// Returns the path written, relative to the sync's folder.
///
/// # Errors
/// [`DeletedError::NotFound`] when `key` names nothing that can be restored;
/// otherwise rclone or the copy failed.
pub async fn restore(
    paths: &Paths,
    rclone: &Rclone,
    pair: &Pair,
    key: &str,
    side: DeletedSide,
) -> Result<String, DeletedError> {
    match side {
        DeletedSide::Local => {
            let source = paths.versions_dir(&pair.id);
            let local = pair.local.clone();
            let key = key.to_owned();
            tokio::task::spawn_blocking(move || restore_local(&source, &local, &key))
                .await
                .map_err(io::Error::other)?
        }
        DeletedSide::Remote => restore_remote(paths, rclone, pair, key).await,
        DeletedSide::Drive => restore_drive(paths, rclone, pair, key).await,
    }
}

fn keep_for(keep_days: u32) -> SignedDuration {
    SignedDuration::from_secs(i64::from(keep_days) * SECS_PER_DAY)
}

fn sort_newest_first(items: &mut [DeletedItem]) {
    items.sort_by(|a, b| b.when.cmp(&a.when).then_with(|| a.path.cmp(&b.path)));
}

/// A file found under the dated folder `stamp` of a versions folder.
fn version_item(
    side: DeletedSide,
    stamp: &str,
    path: String,
    bytes: u64,
    local: &Path,
    keep: SignedDuration,
) -> Option<DeletedItem> {
    let when = timestamp(parse_stamp(stamp)?);
    let kind = if occupied(&local.join(&path)) {
        DeletedKind::Replaced
    } else {
        DeletedKind::Deleted
    };
    Some(DeletedItem {
        side,
        kind,
        key: format!("{stamp}/{path}"),
        path,
        when,
        kept_until: when.and_then(|when| when.checked_add(keep).ok()),
        size: bytes,
    })
}

fn local_items(root: &Path, local: &Path, keep: SignedDuration) -> io::Result<Vec<DeletedItem>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let mut items = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(stamp) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if parse_stamp(&stamp).is_none() || !entry.file_type()?.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_files(&entry.path(), "", &mut files)?;
        items.extend(files.into_iter().filter_map(|(path, size)| {
            version_item(DeletedSide::Local, &stamp, path, size, local, keep)
        }));
    }
    Ok(items)
}

/// Files and symlinks under `dir`, as `/`-joined paths after `prefix`. A
/// name that is not UTF-8 cannot round-trip through a key and is skipped.
fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<(String, u64)>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            log::warn!(
                "skipping a version with a name that is not UTF-8 in {}",
                dir.display()
            );
            continue;
        };
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), &path, out)?;
        } else {
            out.push((path, entry.metadata()?.len()));
        }
    }
    Ok(())
}

fn timestamp(stamp: DateTime) -> Option<Timestamp> {
    stamp
        .to_zoned(TimeZone::system())
        .ok()
        .map(|zoned| zoned.timestamp())
}

async fn remote_items(
    rclone: &Rclone,
    pair: &Pair,
    keep: SignedDuration,
) -> Result<Vec<DeletedItem>, RcloneError> {
    let Some(root) = remote_versions_dir(&pair.remote, &pair.id) else {
        return Ok(Vec::new());
    };
    if rclone.stat(&root).await?.is_none() {
        return Ok(Vec::new());
    }
    let local = pair.local.clone();
    Ok(rclone
        .list_tree(&root)
        .await?
        .into_iter()
        .filter(|entry| !entry.is_dir)
        .filter_map(|entry| {
            let (stamp, path) = entry.path.split_once('/')?;
            let size = u64::try_from(entry.size).unwrap_or(0);
            version_item(
                DeletedSide::Remote,
                stamp,
                path.to_owned(),
                size,
                &local,
                keep,
            )
        })
        .collect())
}

async fn on_drive(rclone: &Rclone, pair: &Pair) -> Result<Option<String>, RcloneError> {
    let Some((account, _)) = split_remote(&pair.remote) else {
        return Ok(None);
    };
    let drive = rclone
        .remote_types()
        .await?
        .iter()
        .any(|(name, kind)| name == account && kind == DRIVE_TYPE);
    Ok(drive.then(|| account.to_owned()))
}

async fn drive_items(rclone: &Rclone, pair: &Pair) -> Result<Vec<DeletedItem>, RcloneError> {
    if on_drive(rclone, pair).await?.is_none() {
        return Ok(Vec::new());
    }
    let mut items: Vec<DeletedItem> = rclone
        .list_trashed(&pair.remote)
        .await?
        .into_iter()
        .filter(|entry| !entry.id.is_empty() && !in_remote_versions(&entry.path))
        .map(|entry| DeletedItem {
            side: DeletedSide::Drive,
            kind: DeletedKind::Deleted,
            path: entry.path,
            when: None,
            kept_until: None,
            size: u64::try_from(entry.size).unwrap_or(0),
            key: entry.id,
        })
        .collect();
    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(items)
}

fn in_remote_versions(path: &str) -> bool {
    path.split('/').next() == Some(REMOTE_VERSIONS_DIR)
}

/// `path` as a relative path, when every part is a plain name.
fn plain_relative(path: &Path) -> Option<PathBuf> {
    let relative: PathBuf = path
        .components()
        .map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (!relative.as_os_str().is_empty()).then_some(relative)
}

/// `<stamp>/<relative path>` split in two, when every part is a plain name
/// and the first is a dated folder.
fn parse_version_key(key: &str) -> Option<(DateTime, PathBuf)> {
    let (stamp, rest) = key.split_once('/')?;
    Some((parse_stamp(stamp)?, plain_relative(Path::new(rest))?))
}

fn restored_name(target: &Path, when: &DateTime) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let (stem, ext) = split_extension(name);
    target.with_file_name(format!(
        "{stem} (restored {}){ext}",
        when.strftime(RESTORED_FORMAT)
    ))
}

/// Copies `source` to `local/relative`, or beside it under a restored name.
fn place(source: &Path, local: &Path, relative: &Path, when: &DateTime) -> io::Result<String> {
    let mut target = local.join(relative);
    if occupied(&target) {
        target = restored_name(&target, when);
    }
    let written = copy_without_overwriting(source, &target)?;
    let relative = written.strip_prefix(local).unwrap_or(&written);
    Ok(relative.to_string_lossy().into_owned())
}

fn restore_local(versions: &Path, local: &Path, key: &str) -> Result<String, DeletedError> {
    let not_found = || DeletedError::NotFound(key.to_owned());
    let (stamp, relative) = parse_version_key(key).ok_or_else(not_found)?;
    let source = versions.join(key);
    match std::fs::symlink_metadata(&source) {
        Ok(meta) if !meta.is_dir() => {}
        Ok(_) => return Err(not_found()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(not_found()),
        Err(err) => return Err(err.into()),
    }
    Ok(place(&source, local, &relative, &stamp)?)
}

async fn restore_remote(
    paths: &Paths,
    rclone: &Rclone,
    pair: &Pair,
    key: &str,
) -> Result<String, DeletedError> {
    let not_found = || DeletedError::NotFound(key.to_owned());
    let (stamp, relative) = parse_version_key(key).ok_or_else(not_found)?;
    let root = remote_versions_dir(&pair.remote, &pair.id).ok_or_else(not_found)?;
    let source = format!("{root}/{key}");
    match rclone.stat(&source).await? {
        Some(entry) if !entry.is_dir => {}
        _ => return Err(not_found()),
    }
    fetch_and_place(paths, pair, relative, stamp, |fetched| async move {
        rclone.copy_file(&source, &fetched).await
    })
    .await
}

async fn restore_drive(
    paths: &Paths,
    rclone: &Rclone,
    pair: &Pair,
    id: &str,
) -> Result<String, DeletedError> {
    let not_found = || DeletedError::NotFound(id.to_owned());
    let account = on_drive(rclone, pair).await?.ok_or_else(not_found)?;
    let entry = rclone
        .list_trashed(&pair.remote)
        .await?
        .into_iter()
        .find(|entry| entry.id == id && !in_remote_versions(&entry.path))
        .ok_or_else(not_found)?;
    let relative = plain_relative(Path::new(&entry.path)).ok_or_else(not_found)?;
    let now = Zoned::now().datetime();
    fetch_and_place(paths, pair, relative, now, |fetched| async move {
        rclone.copy_id(&account, &entry.id, &fetched).await
    })
    .await
}

/// Downloads with `fetch` into syncfast's own data folder first, so a failed
/// or partial download never shows up in the sync's folder.
async fn fetch_and_place<F, Fut>(
    paths: &Paths,
    pair: &Pair,
    relative: PathBuf,
    when: DateTime,
    fetch: F,
) -> Result<String, DeletedError>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = Result<(), RcloneError>>,
{
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let staging = paths.data_dir().join("restoring").join(&pair.id);
    tokio::fs::create_dir_all(&staging).await?;
    let fetched = staging.join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = tokio::fs::remove_file(&fetched).await;
    let fetched_result = fetch(fetched.clone()).await;
    let placed = match fetched_result {
        Ok(()) => {
            let (source, local) = (fetched.clone(), pair.local.clone());
            tokio::task::spawn_blocking(move || place(&source, &local, &relative, &when))
                .await
                .map_err(io::Error::other)
                .and_then(|placed| placed)
                .map_err(DeletedError::from)
        }
        Err(err) => Err(err.into()),
    };
    let _ = tokio::fs::remove_file(&fetched).await;
    placed
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;

    use super::*;

    fn put(root: &Path, relative: &str, content: &str) {
        let file = root.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, content).unwrap();
    }

    fn listed(root: &Path, local: &Path) -> Vec<DeletedItem> {
        let mut items = local_items(root, local, keep_for(30)).unwrap();
        sort_newest_first(&mut items);
        items
    }

    #[test]
    fn local_versions_are_listed_newest_first_and_marked_deleted_or_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (versions, local) = (dir.path().join("v"), dir.path().join("l"));
        put(&versions, "20260301-120000/docs/gone.txt", "old");
        put(&versions, "20260302-120000/kept.txt", "older");
        put(&versions, "notes/ignored.txt", "x");
        put(&local, "kept.txt", "new");

        let items = listed(&versions, &local);

        let seen: Vec<_> = items
            .iter()
            .map(|item| (item.key.as_str(), item.path.as_str(), item.kind))
            .collect();
        assert_eq!(
            seen,
            [
                (
                    "20260302-120000/kept.txt",
                    "kept.txt",
                    DeletedKind::Replaced
                ),
                (
                    "20260301-120000/docs/gone.txt",
                    "docs/gone.txt",
                    DeletedKind::Deleted
                ),
            ]
        );
    }

    #[test]
    fn a_local_version_is_kept_for_the_keep_days_after_its_round() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "20260301-120000/a.txt", "old");

        let items = listed(dir.path(), &dir.path().join("l"));

        let item = &items[0];
        let kept = item.kept_until.unwrap().as_second() - item.when.unwrap().as_second();
        assert_eq!(kept, 30 * SECS_PER_DAY);
        assert_eq!(item.size, 3);
    }

    #[test]
    fn a_missing_versions_folder_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();

        let items = listed(&dir.path().join("none"), dir.path());

        assert!(items.is_empty());
    }

    #[test]
    fn keys_that_climb_out_or_lack_a_dated_folder_are_refused() {
        for key in [
            "20260301-120000/../../etc/passwd",
            "/20260301-120000/a.txt",
            "20260301-120000",
            "20260301-120000/",
            "notes/a.txt",
            "20260301-120000//etc/passwd",
        ] {
            assert!(parse_version_key(key).is_none(), "{key}");
        }
        assert!(parse_version_key("20260301-120000/docs/a.txt").is_some());
    }

    #[test]
    fn a_restored_name_carries_the_date_before_the_extension() {
        let when = date(2026, 3, 1).at(12, 5, 0, 0);

        assert_eq!(
            restored_name(Path::new("/l/docs/a.tar.gz"), &when),
            Path::new("/l/docs/a.tar (restored 2026-03-01 12-05).gz")
        );
        assert_eq!(
            restored_name(Path::new("/l/.env"), &when),
            Path::new("/l/.env (restored 2026-03-01 12-05)")
        );
    }

    #[test]
    fn the_remote_versions_folder_is_not_part_of_the_trash() {
        assert!(in_remote_versions(
            ".syncfast-versions/p/20260301-120000/a.txt"
        ));
        assert!(!in_remote_versions("docs/.syncfast-versions.txt"));
    }
}
