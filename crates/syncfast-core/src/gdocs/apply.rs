//! Carries out a [`Plan`]: the Drive part before rclone runs, so a Doc
//! deleted with its folder goes to the trash before bisync removes the
//! folder, and the disk part after it (ADR 0010).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use jiff::Zoned;
use serde_json::{Value, json};

use super::link;
use super::plan::{self, Action, Plan, WriteKind, render_tracked};
use super::remote::{self, DriveFile, FILE_FIELDS, RemoteFile, files_scope, shown};
use super::state::{Link, Tracked};
use crate::behavior::{Behavior, SyncMode};
use crate::bisync::stamp;
use crate::bisync_log::{Change, ChangeKind, FileError, RunSummary};
use crate::config::{Pair, split_remote};
use crate::conflicts::{copy_without_overwriting, occupied};
use crate::drive_api::{DriveClient, DriveError, Method};
use crate::filters::DirMatcher;
use crate::paths::Paths;
use crate::rclone::{FolderId, Rclone, RcloneError};
use crate::state::StateError;

pub const NOT_LINKED: &str =
    "Not linked to a Google file this account can open, so it stays on this computer.";
const OCCUPIED: &str = "A file is already here, so the link was not written.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `capped`: stop before trashing more than `max_delete` percent.
    Drive {
        capped: bool,
    },
    Disk,
    /// The sync no longer shows Google files: the links it wrote go.
    Cleanup,
    /// Plans the Drive part and reports it, touching nothing.
    DryRun,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocsOutcome {
    pub changes: Vec<Change>,
    pub errors: Vec<FileError>,
    pub uploaded: u32,
    pub downloaded: u32,
    pub deleted_local: u32,
    pub deleted_remote: u32,
    pub renamed: u32,
    /// How many Docs would have gone to the trash when the cap stopped the
    /// Drive phase.
    pub capped: Option<usize>,
}

impl DocsOutcome {
    pub fn merge(&mut self, other: Self) {
        self.changes.extend(other.changes);
        self.errors.extend(other.errors);
        self.uploaded += other.uploaded;
        self.downloaded += other.downloaded;
        self.deleted_local += other.deleted_local;
        self.deleted_remote += other.deleted_remote;
        self.renamed += other.renamed;
        self.capped = self.capped.or(other.capped);
    }

    pub fn add_to(self, summary: &mut RunSummary) {
        let add = |field: &mut Option<u32>, count: u32| {
            if count > 0 {
                *field = Some(field.unwrap_or(0) + count);
            }
        };
        add(&mut summary.uploaded, self.uploaded);
        add(&mut summary.downloaded, self.downloaded);
        add(&mut summary.deleted_local, self.deleted_local);
        add(&mut summary.deleted_remote, self.deleted_remote);
        add(&mut summary.renamed, self.renamed);
        summary.changes.extend(self.changes);
        summary.errors.extend(self.errors);
    }

    fn error(&mut self, path: &str, message: impl Into<String>) {
        self.errors.push(FileError {
            path: path.to_owned(),
            message: message.into(),
        });
    }

    fn change(&mut self, kind: ChangeKind, path: &str, from: Option<&str>) {
        let mut change = Change::new(kind, path);
        change.from = from.map(str::to_owned);
        self.changes.push(change);
    }

    /// The change `action` makes, counted.
    fn record(&mut self, action: &Action) {
        match action {
            Action::Trash { path, .. } => {
                self.deleted_remote += 1;
                self.change(ChangeKind::DeleteRemote, path, None);
            }
            Action::Move { from, to, .. } => {
                self.renamed += 1;
                self.change(ChangeKind::RenameRemote, to, Some(from));
            }
            Action::Copy { path, .. } => {
                self.uploaded += 1;
                self.change(ChangeKind::Upload, path, None);
            }
            Action::Write {
                path,
                kind: WriteKind::New,
                ..
            } => {
                self.downloaded += 1;
                self.change(ChangeKind::Download, path, None);
            }
            Action::Write {
                path,
                kind: WriteKind::Renamed { from },
                ..
            } => {
                self.renamed += 1;
                self.change(ChangeKind::RenameLocal, path, Some(from));
            }
            Action::Remove {
                path, gone: true, ..
            } => {
                self.deleted_local += 1;
                self.change(ChangeKind::DeleteLocal, path, None);
            }
            Action::Warn { path } => self.error(path, NOT_LINKED),
            Action::Write { .. }
            | Action::Remove { .. }
            | Action::KeepEdited { .. }
            | Action::ConflictCopy { .. } => {}
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DocsError {
    #[error("{0}")]
    Drive(#[from] DriveError),
    #[error("{0}")]
    Rclone(#[from] RcloneError),
    #[error("{0}")]
    State(#[from] StateError),
    #[error("cannot read the sync's folder: {0}")]
    Io(#[from] std::io::Error),
}

/// What a phase needs from the round.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub paths: &'a Paths,
    pub rclone: &'a Rclone,
    pub pair: &'a Pair,
    pub behavior: &'a Behavior,
    pub matcher: &'a DirMatcher,
    /// Names the versions folder an edited link is kept in.
    pub now: &'a Zoned,
}

/// Runs `phase` for a sync whose remote is on Drive; any other sync gets
/// an empty outcome.
///
/// # Errors
/// rclone, Drive, the sync's folder or `gdocs.json` fails; nothing of the
/// phase was done past what the outcome would have reported.
pub async fn reconcile(cx: Context<'_>, phase: Phase) -> Result<DocsOutcome, DocsError> {
    if phase == Phase::Cleanup {
        cleanup(cx).await;
        return Ok(DocsOutcome::default());
    }
    let Some((account, _)) = split_remote(&cx.pair.remote) else {
        return Ok(DocsOutcome::default());
    };
    if !cx.rclone.is_drive(account).await? {
        return Ok(DocsOutcome::default());
    }
    let mut client = DriveClient::new(cx.rclone.clone(), account.to_owned());
    let mut folders = client.pair_folders(&cx.pair.remote).await?;
    folders.retain(|folder| !cx.matcher.is_excluded(Path::new(&folder.path)));
    let found = remote::list(&mut client, &folders).await?;
    let mut tracked = Tracked::load(cx.paths, &cx.pair.id, &stamp(cx.now)).await;
    tracked
        .links
        .retain(|_, link| !cx.matcher.is_excluded(Path::new(&link.folder)));
    let root = cx.pair.local.clone();
    let matcher = cx.matcher.clone();
    let local = tokio::task::spawn_blocking(move || link::scan(&root, &matcher))
        .await
        .map_err(std::io::Error::other)??;
    let two_way = cx.behavior.mode == SyncMode::Bisync;
    let plan = plan::plan(&tracked, &local, &found, two_way);
    match phase {
        Phase::DryRun => {
            let mut outcome = DocsOutcome::default();
            for action in plan.actions.iter().filter(|action| action.on_drive()) {
                outcome.record(action);
            }
            Ok(outcome)
        }
        Phase::Drive { capped } => {
            let allowed = tracked.links.len() * usize::from(cx.behavior.max_delete) / 100;
            let trashed = plan.trash_count();
            if capped && trashed > allowed {
                return Ok(DocsOutcome {
                    capped: Some(trashed),
                    ..DocsOutcome::default()
                });
            }
            let mut drive = Drive {
                cx,
                client,
                folders,
                found: &found,
            };
            Ok(drive.apply(plan, tracked).await?)
        }
        Phase::Disk => apply_disk(cx, plan, tracked).await,
        Phase::Cleanup => unreachable!("handled above"),
    }
}

/// `rel` under the sync's remote, as rclone takes it.
pub(crate) fn remote_path(remote: &str, rel: &str) -> String {
    if rel.is_empty() {
        remote.to_owned()
    } else if remote.ends_with(':') || remote.ends_with('/') {
        format!("{remote}{rel}")
    } else {
        format!("{remote}/{rel}")
    }
}

struct Drive<'a> {
    cx: Context<'a>,
    client: DriveClient,
    folders: Vec<FolderId>,
    found: &'a [RemoteFile],
}

impl Drive<'_> {
    async fn apply(&mut self, plan: Plan, mut tracked: Tracked) -> Result<DocsOutcome, DocsError> {
        let mut outcome = DocsOutcome::default();
        let actions: Vec<&Action> = plan.actions.iter().filter(|a| a.on_drive()).collect();
        if actions.is_empty() {
            return Ok(outcome);
        }
        self.create_folders(&actions).await;
        for action in actions {
            let done = match action {
                Action::Trash { id, .. } => self.trash(id).await.map(|()| {
                    tracked.links.remove(id);
                }),
                Action::Move {
                    id, name, folder, ..
                } => self.rename(id, name, folder).await.map(|()| {
                    if let Some(link) = plan.next.links.get(id) {
                        tracked.links.insert(id.clone(), link.clone());
                    }
                }),
                Action::Copy {
                    source,
                    name,
                    folder,
                    path,
                    ..
                } => self
                    .copy(source, name, folder, path)
                    .await
                    .map(|(id, link)| {
                        tracked.links.insert(id, link);
                    }),
                _ => continue,
            };
            match done {
                Ok(()) => outcome.record(action),
                Err(err) => outcome.error(action_path(action), failure(action, &err)),
            }
        }
        tracked.save(self.cx.paths, &self.cx.pair.id).await?;
        Ok(outcome)
    }

    /// Creates on Drive the folders a move or copy goes to that only exist
    /// on disk yet, then reads the folder ids again.
    async fn create_folders(&mut self, actions: &[&Action]) {
        let known: HashSet<&str> = self.folders.iter().map(|f| f.path.as_str()).collect();
        let missing: HashSet<&str> = actions
            .iter()
            .filter_map(|action| match action {
                Action::Move { folder, .. } | Action::Copy { folder, .. } => Some(folder.as_str()),
                _ => None,
            })
            .filter(|folder| !known.contains(folder))
            .collect();
        if missing.is_empty() {
            return;
        }
        for folder in missing {
            let path = remote_path(&self.cx.pair.remote, folder);
            if let Err(err) = self.cx.rclone.mkdir(&path).await {
                log::warn!(
                    "{}: cannot create {folder} on Drive: {err}",
                    self.cx.pair.id
                );
            }
        }
        match self.client.pair_folders(&self.cx.pair.remote).await {
            Ok(folders) => self.folders = folders,
            Err(err) => log::warn!("{}: cannot read the folders again: {err}", self.cx.pair.id),
        }
    }

    fn folder_id(&self, folder: &str) -> Result<String, DriveError> {
        self.folders
            .iter()
            .find(|known| known.path == folder)
            .map(|known| known.id.clone())
            .ok_or_else(|| DriveError::FolderMissing(folder.to_owned()))
    }

    async fn trash(&mut self, id: &str) -> Result<(), DocsError> {
        let scope = files_scope(&self.client, false);
        let _: Value = self
            .client
            .send_json(
                Method::Patch,
                format!("/files/{id}"),
                scope,
                json!({"trashed": true}),
            )
            .await?;
        Ok(())
    }

    async fn rename(&mut self, id: &str, name: &str, folder: &str) -> Result<(), DocsError> {
        let target = self.folder_id(folder)?;
        let mut query = files_scope(&self.client, false);
        if let Some(current) = self.found.iter().find(|file| file.id == id)
            && current.parent != target
        {
            query.extend([
                ("addParents", target),
                ("removeParents", current.parent.clone()),
            ]);
        }
        let _: Value = self
            .client
            .send_json(
                Method::Patch,
                format!("/files/{id}"),
                query,
                json!({"name": name}),
            )
            .await?;
        Ok(())
    }

    /// The copy replaces the copied link at `path`, which then names it.
    async fn copy(
        &mut self,
        source: &str,
        name: &str,
        folder: &str,
        path: &str,
    ) -> Result<(String, Link), DocsError> {
        let parent = self.folder_id(folder)?;
        let mut query = files_scope(&self.client, false);
        query.push(("fields", FILE_FIELDS.to_owned()));
        let copied: DriveFile = self
            .client
            .send_json(
                Method::Post,
                format!("/files/{source}/copy"),
                query,
                json!({"name": name, "parents": [parent]}),
            )
            .await?;
        let paths: HashMap<&str, &str> = self
            .folders
            .iter()
            .map(|known| (known.id.as_str(), known.path.as_str()))
            .collect();
        let file = shown(copied, &paths).ok_or(DriveError::BadAnswer)?;
        let target = self.cx.pair.local.join(path);
        let text = plan::render_remote(&file);
        let one_way = self.cx.behavior.mode != SyncMode::Bisync;
        tokio::task::spawn_blocking(move || write_link(&target, &text, one_way))
            .await
            .map_err(std::io::Error::other)??;
        let link = Link {
            path: path.to_owned(),
            name: file.name,
            folder: file.folder,
            mime: file.mime,
            url: file.url,
            number: None,
            shortcut: file.shortcut,
        };
        Ok((file.id, link))
    }
}

fn action_path(action: &Action) -> &str {
    match action {
        Action::Trash { path, .. }
        | Action::Copy { path, .. }
        | Action::Write { path, .. }
        | Action::Remove { path, .. }
        | Action::KeepEdited { path, .. }
        | Action::Warn { path } => path,
        Action::Move { to, .. } => to,
        Action::ConflictCopy { from, .. } => from,
    }
}

/// A copy of a file this account cannot open is the user's own link, not
/// a failure.
fn failure(action: &Action, err: &DocsError) -> String {
    let refused = matches!(
        err,
        DocsError::Drive(DriveError::Http {
            status: 403 | 404,
            ..
        })
    );
    if matches!(action, Action::Copy { foreign: true, .. }) && refused {
        NOT_LINKED.to_owned()
    } else {
        err.to_string()
    }
}

fn write_link(path: &Path, text: &str, read_only: bool) -> std::io::Result<()> {
    fastframe_file::write(path, text)?;
    if read_only {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))?;
    }
    Ok(())
}

/// `<path>.conflict<N>` with the lowest free `N`.
fn conflict_path(path: &Path) -> PathBuf {
    let mut number = 1_u32;
    loop {
        let mut candidate = path.as_os_str().to_owned();
        candidate.push(format!(".conflict{number}"));
        let candidate = PathBuf::from(candidate);
        if !occupied(&candidate) {
            return candidate;
        }
        number += 1;
    }
}

struct Disk {
    root: PathBuf,
    versions: PathBuf,
    read_only: bool,
}

impl Disk {
    fn run(&self, action: &Action) -> std::io::Result<()> {
        match action {
            Action::KeepEdited { path, .. } => {
                copy_without_overwriting(&self.root.join(path), &self.versions.join(path))
                    .map(|_| ())
            }
            Action::ConflictCopy { from, to, .. } => {
                let target = conflict_path(&self.root.join(to));
                std::fs::rename(self.root.join(from), target)
            }
            Action::Remove { path, .. } => match std::fs::remove_file(self.root.join(path)) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
                _ => Ok(()),
            },
            Action::Write {
                path, text, kind, ..
            } => {
                let target = self.root.join(path);
                if *kind != WriteKind::Refresh && occupied(&target) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        OCCUPIED,
                    ));
                }
                write_link(&target, text, self.read_only)
            }
            Action::Trash { .. }
            | Action::Move { .. }
            | Action::Copy { .. }
            | Action::Warn { .. } => Ok(()),
        }
    }
}

async fn apply_disk(
    cx: Context<'_>,
    plan: Plan,
    tracked: Tracked,
) -> Result<DocsOutcome, DocsError> {
    let disk = Disk {
        root: cx.pair.local.clone(),
        versions: cx.paths.versions_dir(&cx.pair.id).join(stamp(cx.now)),
        read_only: cx.behavior.mode != SyncMode::Bisync,
    };
    let Plan { actions, mut next } = plan;
    let (outcome, failed) = tokio::task::spawn_blocking(move || {
        let mut outcome = DocsOutcome::default();
        let mut failed: HashSet<String> = HashSet::new();
        for action in &actions {
            if action.on_drive() {
                failed.extend(action.tracked_id().map(str::to_owned));
                continue;
            }
            if action.tracked_id().is_some_and(|id| failed.contains(id)) {
                continue;
            }
            match disk.run(action) {
                Ok(()) => outcome.record(action),
                Err(err) => {
                    failed.extend(action.tracked_id().map(str::to_owned));
                    outcome.error(action_path(action), err.to_string());
                }
            }
        }
        (outcome, failed)
    })
    .await
    .map_err(std::io::Error::other)?;
    for id in failed {
        match tracked.links.get(&id) {
            Some(link) => next.links.insert(id, link.clone()),
            None => next.links.remove(&id),
        };
    }
    next.save(cx.paths, &cx.pair.id).await?;
    Ok(outcome)
}

/// Deletes each link the sync wrote that is still as written, then
/// forgets them. An edited link stays.
async fn cleanup(cx: Context<'_>) {
    if !Tracked::exists(cx.paths, &cx.pair.id).await {
        return;
    }
    let tracked = Tracked::load(cx.paths, &cx.pair.id, &stamp(cx.now)).await;
    let root = cx.pair.local.clone();
    let id = cx.pair.id.clone();
    let swept = tokio::task::spawn_blocking(move || {
        for (drive_id, link) in &tracked.links {
            let path = root.join(&link.path);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if text == render_tracked(drive_id, link)
                && let Err(err) = std::fs::remove_file(&path)
            {
                log::warn!("{id}: cannot delete {}: {err}", path.display());
            }
        }
    })
    .await;
    if let Err(err) = swept {
        log::warn!("{}: Google Docs cleanup failed: {err}", cx.pair.id);
        return;
    }
    Tracked::remove(cx.paths, &cx.pair.id).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths_join_the_sync_folder() {
        assert_eq!(remote_path("gd:Notes", "a/b"), "gd:Notes/a/b");
        assert_eq!(remote_path("gd:", "a"), "gd:a");
        assert_eq!(remote_path("gd:Notes", ""), "gd:Notes");
    }

    #[test]
    fn a_trashed_doc_and_a_warning_reach_the_round_summary() {
        let mut outcome = DocsOutcome::default();
        outcome.record(&Action::Trash {
            id: "d1".into(),
            path: "a/Ata.gdoc.html".into(),
        });
        outcome.record(&Action::Warn {
            path: "Hand.gdoc.html".into(),
        });
        let mut summary = RunSummary {
            deleted_remote: Some(2),
            ..RunSummary::default()
        };

        outcome.add_to(&mut summary);

        assert_eq!(summary.deleted_remote, Some(3));
        assert_eq!(
            summary.changes,
            [Change::new(ChangeKind::DeleteRemote, "a/Ata.gdoc.html")]
        );
        assert_eq!(summary.errors[0].message, NOT_LINKED);
    }

    #[test]
    fn a_conflict_copy_takes_the_lowest_free_number() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Ata.gdoc.html");
        std::fs::write(dir.path().join("Ata.gdoc.html.conflict1"), "x").unwrap();

        assert_eq!(
            conflict_path(&target),
            dir.path().join("Ata.gdoc.html.conflict2")
        );
    }

    #[test]
    fn a_link_written_for_a_one_way_sync_is_read_only_and_can_be_rewritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/Ata.gdoc.html");

        write_link(&path, "one", true).unwrap();
        write_link(&path, "two", true).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o444);
    }
}
