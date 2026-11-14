//! Reads the JSON log `rclone bisync --use-json-log -v` writes and turns it
//! into what the UI shows: per-file changes, counters and live progress.
//!
//! Every message this module recognises comes from the fixtures in
//! `tests/fixtures/rclone-1.76.0/`; side attribution is explained on
//! [`summarize`].

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

const MSG_BISYNC_SUCCESSFUL: &str = "Bisync successful";
const MSG_COPIED_NEW: &str = "Copied (new)";
const MSG_COPIED_REPLACED: &str = "Copied (replaced existing)";
const MSG_MOVED: &str = "Moved (server-side)";
const MSG_DELETED: &str = "Deleted";
const MSG_MOVED_TO: &str = "Moved (server-side) to: ";
const MSG_RENAMED_FROM: &str = "Renamed from ";
const MSG_MOVED_TO_BACKUP: &str = "Moved into backup dir";
const SKIPPED_COPY: &str = "copy";
const SKIPPED_MOVE: &str = "move";
const SKIPPED_MOVE_TO: &str = "move to ";
const SKIPPED_DELETE: &str = "delete";
const SKIPPED_MOVE_TO_BACKUP: &str = "move into backup dir";
const SKIPPED_MAKE_DIR: &str = "make directory";
const SKIPPED_REMOVE_DIR: &str = "remove directory";
const SKIPPED_SET_DIR_TIME: &str = "set directory modification time";
const NEW_FILE: &str = "File is new";
const CHANGED_FILE: &str = "File changed:";
const DELETED_FILE: &str = "File was deleted";
const QUEUED_COPIES: &str = "Do queued copies to";
const RESYNC_COPIES: &str = "Resync is copying files to";
const CONFLICT_WARNING: &str = "New or changed in both paths";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    /// The line was not JSON or carried no known level.
    Unknown,
}

impl Level {
    fn parse(text: &str) -> Self {
        match text {
            "debug" => Self::Debug,
            "info" => Self::Info,
            "notice" => Self::Notice,
            "warning" => Self::Warning,
            "error" => Self::Error,
            "critical" | "alert" | "emerg" => Self::Critical,
            _ => Self::Unknown,
        }
    }

    /// Lines that may carry the reason a run failed.
    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Error | Self::Critical | Self::Unknown)
    }
}

/// Live numbers from rclone's `--stats` lines. Field names follow the
/// `stats` object in the `stats` fixture.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub transfers: u64,
    pub total_transfers: u64,
    pub bytes: u64,
    pub total_bytes: u64,
    pub speed: f64,
    pub eta_seconds: Option<u64>,
    /// First file rclone reports as transferring.
    pub current: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    pub level: Level,
    pub msg: String,
    pub object: Option<String>,
    pub object_type: Option<String>,
    /// The operation rclone skipped because of `--dry-run`.
    pub skipped: Option<String>,
    pub progress: Option<Progress>,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(default)]
    level: String,
    #[serde(default)]
    msg: String,
    object: Option<String>,
    #[serde(rename = "objectType")]
    object_type: Option<String>,
    skipped: Option<String>,
    stats: Option<RawStats>,
}

#[derive(Deserialize)]
struct RawStats {
    #[serde(default)]
    transfers: u64,
    #[serde(default, rename = "totalTransfers")]
    total_transfers: u64,
    #[serde(default)]
    bytes: u64,
    #[serde(default, rename = "totalBytes")]
    total_bytes: u64,
    #[serde(default)]
    speed: f64,
    eta: Option<u64>,
    #[serde(default)]
    transferring: Vec<RawTransfer>,
}

#[derive(Deserialize)]
struct RawTransfer {
    name: String,
}

impl LogLine {
    /// A line that is not JSON keeps its text as `msg` with
    /// [`Level::Unknown`], so classification still sees it.
    #[must_use]
    pub fn parse(line: &str) -> Self {
        let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
            return Self {
                level: Level::Unknown,
                msg: line.to_owned(),
                object: None,
                object_type: None,
                skipped: None,
                progress: None,
            };
        };
        Self {
            level: Level::parse(&raw.level),
            msg: raw.msg,
            object: raw.object,
            object_type: raw.object_type,
            skipped: raw.skipped,
            progress: raw.stats.map(|stats| Progress {
                transfers: stats.transfers,
                total_transfers: stats.total_transfers,
                bytes: stats.bytes,
                total_bytes: stats.total_bytes,
                speed: stats.speed,
                eta_seconds: stats.eta,
                current: stats.transferring.into_iter().next().map(|t| t.name),
            }),
        }
    }
}

/// Parses every non-empty line of a log.
#[must_use]
pub fn parse_log(text: &str) -> Vec<LogLine> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(LogLine::parse)
        .collect()
}

/// # Errors
/// The file cannot be read.
pub async fn read_log(path: &Path) -> std::io::Result<Vec<LogLine>> {
    let bytes = tokio::fs::read(path).await?;
    Ok(parse_log(&String::from_utf8_lossy(&bytes)))
}

/// The newest `--stats` reading, `None` when rclone has not printed one yet.
#[must_use]
pub fn last_stats(lines: &[LogLine]) -> Option<Progress> {
    lines.iter().rev().find_map(|line| line.progress.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Upload,
    Download,
    OverwriteLocal,
    OverwriteRemote,
    DeleteLocal,
    DeleteRemote,
    /// A file moved to a new path on this computer; `from` holds the old one.
    RenameLocal,
    /// A file moved to a new path on the remote; `from` holds the old one.
    RenameRemote,
    Other,
}

impl ChangeKind {
    #[must_use]
    pub fn is_rename(self) -> bool {
        matches!(self, Self::RenameLocal | Self::RenameRemote)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub kind: ChangeKind,
    /// Relative to the synced folder. Folders end with `/`.
    pub path: String,
    /// The path before a rename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

impl Change {
    #[must_use]
    pub fn new(kind: ChangeKind, path: impl Into<String>) -> Self {
        Self {
            kind,
            path: path.into(),
            from: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunSummary {
    pub uploaded: Option<u32>,
    pub downloaded: Option<u32>,
    pub deleted_local: Option<u32>,
    pub deleted_remote: Option<u32>,
    pub renamed: Option<u32>,
    pub conflicts_new: Option<u32>,
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Side {
    Local,
    Remote,
}

impl Side {
    fn opposite(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "Path1" => Some(Self::Local),
            "Path2" => Some(Self::Remote),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detected {
    New,
    Changed,
    Deleted,
}

/// `- Path1   <text>` becomes `(Local, "<text>")`.
fn side_line(msg: &str) -> Option<(Side, &str)> {
    let rest = msg.strip_prefix("- ")?;
    let (side, text) = rest.split_once(char::is_whitespace)?;
    Some((Side::parse(side)?, text.trim_start()))
}

/// `- Path1  File is new   - a.txt`: what bisync found on one side before
/// it decides what to apply.
fn detection(msg: &str) -> Option<(Side, Detected, &str)> {
    let (side, text) = side_line(msg)?;
    let (detected, after) = [
        (NEW_FILE, Detected::New),
        (CHANGED_FILE, Detected::Changed),
        (DELETED_FILE, Detected::Deleted),
    ]
    .into_iter()
    .find_map(|(prefix, detected)| Some((detected, text.strip_prefix(prefix)?)))?;
    let (_, path) = after.split_once(" - ")?;
    Some((side, detected, path))
}

/// `- Path2  Do queued copies to   - Path1` (and the resync variant) names
/// the side the following operations write to.
fn direction(msg: &str) -> Option<Side> {
    let (_, text) = side_line(msg)?;
    let after = text
        .strip_prefix(QUEUED_COPIES)
        .or_else(|| text.strip_prefix(RESYNC_COPIES))?;
    Side::parse(after.trim_start().strip_prefix("- ")?)
}

fn is_conflict_warning(msg: &str) -> bool {
    msg.strip_prefix("- WARNING")
        .is_some_and(|text| text.trim_start().starts_with(CONFLICT_WARNING))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Copy {
        replaced: bool,
    },
    /// The old version moving into `--backup-dir1` before an overwrite.
    BackupMove,
    Delete,
    /// A deletion that went into `--backup-dir1` instead of being removed.
    BackupDelete,
}

fn operation(line: &LogLine) -> Option<Op> {
    if line
        .object_type
        .as_deref()
        .is_some_and(|kind| kind.contains("Dir"))
    {
        return None;
    }
    match (line.skipped.as_deref(), line.msg.as_str()) {
        (Some(SKIPPED_COPY), _) | (None, MSG_COPIED_NEW) => Some(Op::Copy { replaced: false }),
        (None, MSG_COPIED_REPLACED) => Some(Op::Copy { replaced: true }),
        (Some(SKIPPED_MOVE), _) | (None, MSG_MOVED) => Some(Op::BackupMove),
        (Some(SKIPPED_DELETE), _) | (None, MSG_DELETED) => Some(Op::Delete),
        (Some(SKIPPED_MOVE_TO_BACKUP), _) | (None, MSG_MOVED_TO_BACKUP) => Some(Op::BackupDelete),
        _ => None,
    }
}

#[derive(Default)]
struct Tracker {
    target: Option<Side>,
    detected: HashMap<(Side, String), Detected>,
    backed_up: HashSet<String>,
    changes: Vec<Change>,
    seen: HashSet<(ChangeKind, String)>,
    conflicts: usize,
    completed: bool,
}

impl Tracker {
    fn push(&mut self, kind: ChangeKind, path: String) {
        self.push_change(Change::new(kind, path));
    }

    fn push_change(&mut self, change: Change) {
        if self.seen.insert((change.kind, change.path.clone())) {
            self.changes.push(change);
        }
    }

    fn feed(&mut self, line: &LogLine) {
        if line.msg == MSG_BISYNC_SUCCESSFUL {
            self.completed = true;
        } else if let Some((side, detected, path)) = detection(&line.msg) {
            self.detected.insert((side, path.to_owned()), detected);
        } else if let Some(target) = direction(&line.msg) {
            self.target = Some(target);
        } else if is_conflict_warning(&line.msg) {
            self.conflicts += 1;
        } else if let Some(object) = line.object.as_deref() {
            self.feed_object(line, object);
        }
    }

    fn feed_object(&mut self, line: &LogLine, object: &str) {
        if let Some(from) = renamed_from(&line.msg) {
            let kind = match self.target {
                Some(Side::Local) => ChangeKind::RenameLocal,
                Some(Side::Remote) => ChangeKind::RenameRemote,
                None => ChangeKind::Other,
            };
            let mut change = Change::new(kind, object);
            change.from = kind.is_rename().then_some(from);
            self.push_change(change);
            return;
        }
        if is_rename_move(line) {
            return;
        }
        let Some(op) = operation(line) else {
            self.feed_other(line, object);
            return;
        };
        match (op, self.target) {
            (Op::BackupMove, _) | (Op::Delete | Op::BackupDelete, None) => {
                self.backed_up.insert(object.to_owned());
            }
            (Op::Copy { replaced }, Some(target)) => {
                let backed_up = self.backed_up.remove(object);
                let detected_at =
                    |side: Side| self.detected.get(&(side, object.to_owned())).copied();
                let changed_at_source = detected_at(target.opposite()) == Some(Detected::Changed)
                    && detected_at(target) != Some(Detected::Deleted);
                let kind = match (target, replaced || backed_up || changed_at_source) {
                    (Side::Remote, false) => ChangeKind::Upload,
                    (Side::Remote, true) => ChangeKind::OverwriteRemote,
                    (Side::Local, false) => ChangeKind::Download,
                    (Side::Local, true) => ChangeKind::OverwriteLocal,
                };
                self.push(kind, object.to_owned());
            }
            (Op::Delete | Op::BackupDelete, Some(target)) => {
                self.backed_up.remove(object);
                let kind = match target {
                    Side::Local => ChangeKind::DeleteLocal,
                    Side::Remote => ChangeKind::DeleteRemote,
                };
                self.push(kind, object.to_owned());
            }
            (Op::Copy { .. }, None) => {
                self.push(ChangeKind::Other, object.to_owned());
            }
        }
    }

    /// Dry-run skips of operations this module does not map, and notices
    /// about filesystem objects. Folder housekeeping that only adjusts a
    /// timestamp is not a change.
    fn feed_other(&mut self, line: &LogLine, object: &str) {
        match line.skipped.as_deref() {
            Some(SKIPPED_SET_DIR_TIME) => {}
            Some(SKIPPED_MAKE_DIR | SKIPPED_REMOVE_DIR) => {
                self.push(ChangeKind::Other, format!("{object}/"));
            }
            Some(_) => self.push(ChangeKind::Other, object.to_owned()),
            None => {
                let is_fs_object = line
                    .object_type
                    .as_deref()
                    .is_some_and(|kind| kind.starts_with('*'));
                if line.level >= Level::Notice && line.level < Level::Error && is_fs_object {
                    self.push(ChangeKind::Other, object.to_owned());
                }
            }
        }
    }

    fn finish(mut self) -> RunSummary {
        drop_folders_of_renames(&mut self.changes);
        let count = |kinds: &[ChangeKind]| {
            let n = self
                .changes
                .iter()
                .filter(|change| kinds.contains(&change.kind))
                .count();
            self.completed.then(|| u32::try_from(n).unwrap_or(u32::MAX))
        };
        RunSummary {
            uploaded: count(&[ChangeKind::Upload, ChangeKind::OverwriteRemote]),
            downloaded: count(&[ChangeKind::Download, ChangeKind::OverwriteLocal]),
            deleted_local: count(&[ChangeKind::DeleteLocal]),
            deleted_remote: count(&[ChangeKind::DeleteRemote]),
            renamed: count(&[ChangeKind::RenameLocal, ChangeKind::RenameRemote]),
            conflicts_new: self
                .completed
                .then(|| u32::try_from(self.conflicts).unwrap_or(u32::MAX)),
            changes: self.changes,
        }
    }
}

/// The old path in rclone's `Renamed from "<old>"` notice, which it logs on
/// the new path once a tracked rename is applied (or would be, in a dry run).
pub(crate) fn renamed_from(msg: &str) -> Option<String> {
    crate::listing::unquote(msg.strip_prefix(MSG_RENAMED_FROM)?)
}

/// The move half of a tracked rename, logged on the old path. The
/// `Renamed from` notice on the new path is the one that records it.
fn is_rename_move(line: &LogLine) -> bool {
    line.skipped
        .as_deref()
        .is_some_and(|skipped| skipped.starts_with(SKIPPED_MOVE_TO))
        || line.msg.starts_with(MSG_MOVED_TO)
}

/// A dry run lists making the new folder and removing the old one of a
/// renamed folder; the renamed files already say it.
fn drop_folders_of_renames(changes: &mut Vec<Change>) {
    let renamed: Vec<String> = changes
        .iter()
        .filter(|change| change.kind.is_rename())
        .flat_map(|change| [Some(change.path.clone()), change.from.clone()])
        .flatten()
        .collect();
    if renamed.is_empty() {
        return;
    }
    changes.retain(|change| {
        change.kind != ChangeKind::Other
            || !change.path.ends_with('/')
            || !renamed.iter().any(|path| path.starts_with(&change.path))
    });
}

/// What a run did, from its log (real or `--dry-run`). Side attribution,
/// overwrite detection and when counters are `None`: ADR 0005.
#[must_use]
pub fn summarize(lines: &[LogLine]) -> RunSummary {
    let mut tracker = Tracker::default();
    for line in lines {
        tracker.feed(line);
    }
    tracker.finish()
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::path::PathBuf;

    use super::{LogLine, parse_log};

    pub fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rclone-1.76.0")
            .join(name)
    }

    pub fn fixture(name: &str) -> Vec<LogLine> {
        let text = std::fs::read_to_string(fixture_path(&format!("{name}.jsonl"))).unwrap();
        parse_log(&text)
    }

    pub fn exit_code(name: &str) -> i32 {
        std::fs::read_to_string(fixture_path(&format!("{name}.exit")))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::fixture;
    use super::*;

    fn sorted(mut changes: Vec<Change>) -> Vec<Change> {
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }

    fn change(kind: ChangeKind, path: &str) -> Change {
        Change::new(kind, path)
    }

    fn renames(kind: ChangeKind) -> Vec<Change> {
        let mut changes: Vec<Change> = (1..=10)
            .map(|n| Change {
                kind,
                path: format!("1-disc/f{n}.txt"),
                from: Some(format!("disc/f{n}.txt")),
            })
            .collect();
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }

    fn counters(summary: &RunSummary) -> [Option<u32>; 5] {
        [
            summary.uploaded,
            summary.downloaded,
            summary.deleted_local,
            summary.deleted_remote,
            summary.conflicts_new,
        ]
    }

    #[test]
    fn parse_keeps_level_message_object_and_skipped_operation() {
        let line = LogLine::parse(
            r#"{"level":"notice","msg":"Skipped copy as --dry-run is set (size 7)","skipped":"copy","object":"a.txt","objectType":"*local.Object"}"#,
        );

        assert_eq!(line.level, Level::Notice);
        assert_eq!(line.msg, "Skipped copy as --dry-run is set (size 7)");
        assert_eq!(line.skipped.as_deref(), Some("copy"));
        assert_eq!(line.object.as_deref(), Some("a.txt"));
        assert_eq!(line.object_type.as_deref(), Some("*local.Object"));
        assert_eq!(line.progress, None);
    }

    #[test]
    fn parse_of_a_line_that_is_not_json_keeps_the_text_with_unknown_level() {
        let line = LogLine::parse("Failed to create file system: couldn't fetch token");

        assert_eq!(line.level, Level::Unknown);
        assert_eq!(
            line.msg,
            "Failed to create file system: couldn't fetch token"
        );
    }

    #[test]
    fn parse_log_skips_blank_lines() {
        let lines = parse_log(
            "{\"level\":\"info\",\"msg\":\"a\"}\n\n  \n{\"level\":\"info\",\"msg\":\"b\"}\n",
        );

        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn summarize_of_a_first_run_dry_run_lists_downloads_uploads_and_the_replaced_local_file() {
        let summary = summarize(&fixture("resync_dry_run"));

        assert_eq!(
            sorted(summary.changes.clone()),
            [
                change(ChangeKind::Upload, "a.txt"),
                change(ChangeKind::Upload, "b.txt"),
                change(ChangeKind::Download, "c.txt"),
                change(ChangeKind::OverwriteLocal, "d.txt"),
            ]
        );
        assert_eq!(
            counters(&summary),
            [Some(2), Some(2), Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn summarize_of_the_real_first_run_matches_its_dry_run() {
        let dry = summarize(&fixture("resync_dry_run"));
        let real = summarize(&fixture("resync"));

        assert_eq!(sorted(real.changes), sorted(dry.changes));
        assert_eq!(real.uploaded, dry.uploaded);
        assert_eq!(real.downloaded, dry.downloaded);
    }

    #[test]
    fn summarize_of_a_new_local_file_is_one_upload() {
        let summary = summarize(&fixture("upload"));

        assert_eq!(summary.changes, [change(ChangeKind::Upload, "up.txt")]);
        assert_eq!(
            counters(&summary),
            [Some(1), Some(0), Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn summarize_of_a_new_remote_file_is_one_download() {
        let summary = summarize(&fixture("download"));

        assert_eq!(summary.changes, [change(ChangeKind::Download, "down.txt")]);
        assert_eq!(
            counters(&summary),
            [Some(0), Some(1), Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn summarize_of_a_remote_edit_is_an_overwrite_of_the_local_file() {
        let summary = summarize(&fixture("overwrite_local"));

        assert_eq!(
            summary.changes,
            [change(ChangeKind::OverwriteLocal, "doc.txt")]
        );
        assert_eq!(summary.downloaded, Some(1));
    }

    #[test]
    fn summarize_of_a_local_edit_is_an_overwrite_of_the_remote_file() {
        let summary = summarize(&fixture("overwrite_remote"));

        assert_eq!(
            summary.changes,
            [change(ChangeKind::OverwriteRemote, "doc.txt")]
        );
        assert_eq!(summary.uploaded, Some(1));
    }

    #[test]
    fn summarize_of_a_conflict_counts_it_and_keeps_the_loser_as_an_upload() {
        let summary = summarize(&fixture("conflict"));

        assert_eq!(
            summary.changes,
            [
                change(ChangeKind::OverwriteLocal, "doc.txt"),
                change(ChangeKind::Upload, "doc.txt.conflict1"),
            ]
        );
        assert_eq!(summary.conflicts_new, Some(1));
    }

    #[test]
    fn summarize_of_a_conflict_with_no_winner_keeps_both_renamed_copies() {
        let summary = summarize(&fixture("conflict_none"));

        assert_eq!(
            summary.changes,
            [
                change(ChangeKind::Download, "doc.txt.conflict2"),
                change(ChangeKind::Upload, "doc.txt.conflict1"),
            ]
        );
        assert_eq!(summary.conflicts_new, Some(1));
    }

    #[test]
    fn summarize_of_a_pathname_conflict_names_the_loser_after_its_side() {
        let summary = summarize(&fixture("conflict_pathname"));

        assert_eq!(
            summary.changes,
            [
                change(ChangeKind::OverwriteLocal, "doc.txt"),
                change(ChangeKind::Upload, "doc.txt.path1"),
            ]
        );
        assert_eq!(summary.conflicts_new, Some(1));
    }

    #[test]
    fn summarize_of_a_pathname_conflict_with_no_winner_renames_both_sides() {
        let summary = summarize(&fixture("conflict_pathname_none"));

        assert_eq!(
            summary.changes,
            [
                change(ChangeKind::Download, "doc.txt.path2"),
                change(ChangeKind::Upload, "doc.txt.path1"),
            ]
        );
        assert_eq!(summary.conflicts_new, Some(1));
    }

    #[test]
    fn summarize_of_a_conflict_that_deletes_the_loser_counts_it_but_lists_one_overwrite() {
        let summary = summarize(&fixture("conflict_delete"));

        assert_eq!(
            summary.changes,
            [change(ChangeKind::OverwriteLocal, "doc.txt")]
        );
        assert_eq!(
            counters(&summary),
            [Some(0), Some(1), Some(0), Some(0), Some(1)]
        );
    }

    #[test]
    fn summarize_of_a_conflict_won_by_the_local_side_overwrites_the_remote_file() {
        let summary = summarize(&fixture("conflict_local_wins"));

        assert_eq!(
            summary.changes,
            [
                change(ChangeKind::Download, "doc.txt.conflict1"),
                change(ChangeKind::OverwriteRemote, "doc.txt"),
            ]
        );
        assert_eq!(summary.conflicts_new, Some(1));
    }

    #[test]
    fn summarize_of_a_local_deletion_is_a_remote_deletion() {
        let summary = summarize(&fixture("delete_remote"));

        assert_eq!(
            summary.changes,
            [change(ChangeKind::DeleteRemote, "f1.txt")]
        );
        assert_eq!(
            counters(&summary),
            [Some(0), Some(0), Some(0), Some(1), Some(0)]
        );
    }

    #[test]
    fn summarize_of_a_remote_deletion_is_a_local_deletion() {
        let summary = summarize(&fixture("delete_local"));

        assert_eq!(summary.changes, [change(ChangeKind::DeleteLocal, "f1.txt")]);
        assert_eq!(
            counters(&summary),
            [Some(0), Some(0), Some(1), Some(0), Some(0)]
        );
    }

    #[test]
    fn summarize_of_a_force_dry_run_lists_every_remote_deletion() {
        let summary = summarize(&fixture("force_dry_run"));

        assert_eq!(
            sorted(summary.changes),
            [
                change(ChangeKind::DeleteRemote, "f1.txt"),
                change(ChangeKind::DeleteRemote, "f2.txt"),
                change(ChangeKind::DeleteRemote, "f3.txt"),
            ]
        );
        assert_eq!(summary.deleted_remote, Some(3));
    }

    #[test]
    fn summarize_of_changes_on_both_sides_attributes_each_kind_to_its_side_in_real_and_dry_runs() {
        let expected = vec![
            change(ChangeKind::DeleteRemote, "f1.txt"),
            change(ChangeKind::OverwriteRemote, "f2.txt"),
            change(ChangeKind::DeleteLocal, "f3.txt"),
            change(ChangeKind::OverwriteLocal, "f4.txt"),
            change(ChangeKind::Upload, "n1.txt"),
            change(ChangeKind::Download, "n2.txt"),
        ];

        let real = summarize(&fixture("mixed"));
        let dry = summarize(&fixture("mixed_dry_run"));

        assert_eq!(sorted(real.changes.clone()), expected);
        assert_eq!(
            counters(&real),
            [Some(2), Some(2), Some(1), Some(1), Some(0)]
        );
        assert_eq!(
            counters(&dry),
            [Some(2), Some(2), Some(1), Some(1), Some(0)]
        );
        assert_eq!(sorted(dry.changes), sorted(real.changes));
    }

    #[test]
    fn summarize_of_a_run_with_nothing_to_do_has_zero_counters_and_no_changes() {
        let summary = summarize(&fixture("no_changes"));

        assert_eq!(summary.changes, []);
        assert_eq!(
            counters(&summary),
            [Some(0), Some(0), Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn summarize_of_new_folders_counts_only_the_file() {
        let summary = summarize(&fixture("new_dirs"));

        assert_eq!(
            summary.changes,
            [change(ChangeKind::Upload, "dir/nested.txt")]
        );
        assert_eq!(summary.uploaded, Some(1));
    }

    #[test]
    fn summarize_of_new_folders_in_a_dry_run_lists_them_once_as_other_changes() {
        let summary = summarize(&fixture("new_dirs_dry_run"));

        assert_eq!(
            summary.changes,
            [
                change(ChangeKind::Other, "dir/"),
                change(ChangeKind::Other, "empty/"),
                change(ChangeKind::Upload, "dir/nested.txt"),
                change(ChangeKind::Other, "empty/inner/"),
            ]
        );
        assert_eq!(summary.uploaded, Some(1));
    }

    #[test]
    fn summarize_of_a_run_aborted_for_too_many_deletes_has_no_counters_and_no_changes() {
        let summary = summarize(&fixture("too_many_deletes"));

        assert_eq!(summary, RunSummary::default());
    }

    #[test]
    fn summarize_of_a_critical_failure_has_no_counters_and_ignores_the_error_lines() {
        let summary = summarize(&fixture("critical"));

        assert_eq!(summary, RunSummary::default());
    }

    #[test]
    fn summarize_of_a_folder_renamed_locally_is_ten_remote_renames_and_nothing_else() {
        let summary = summarize(&fixture("rename_local"));

        assert_eq!(
            sorted(summary.changes.clone()),
            renames(ChangeKind::RenameRemote)
        );
        assert_eq!(summary.renamed, Some(10));
        assert_eq!(
            counters(&summary),
            [Some(0), Some(0), Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn summarize_of_a_folder_renamed_on_the_remote_is_ten_local_renames() {
        let summary = summarize(&fixture("rename_remote"));

        assert_eq!(
            sorted(summary.changes.clone()),
            renames(ChangeKind::RenameLocal)
        );
        assert_eq!(summary.renamed, Some(10));
        assert_eq!(
            counters(&summary),
            [Some(0), Some(0), Some(0), Some(0), Some(0)]
        );
    }

    #[test]
    fn summarize_of_a_rename_dry_run_lists_the_renames_without_folder_noise() {
        let summary = summarize(&fixture("rename_dry_run_force"));

        assert_eq!(
            sorted(summary.changes.clone()),
            renames(ChangeKind::RenameRemote)
        );
        assert_eq!(summary.renamed, Some(10));
    }

    #[test]
    fn a_rename_notice_with_an_escaped_old_path_keeps_the_real_name() {
        let log = synthetic(&[
            ("- Path1  Do queued copies to   - Path2", None),
            (r#"Renamed from "old \"q\".txt""#, Some("new.txt")),
            ("Bisync successful", None),
        ]);

        let summary = summarize(&log);

        assert_eq!(summary.changes[0].from.as_deref(), Some("old \"q\".txt"));
    }

    #[test]
    fn a_change_serializes_from_only_for_renames() {
        let plain = serde_json::to_value(change(ChangeKind::Upload, "a")).unwrap();
        let parsed: Change = serde_json::from_value(plain.clone()).unwrap();

        assert_eq!(plain, serde_json::json!({"kind": "upload", "path": "a"}));
        assert_eq!(parsed.from, None);
    }

    fn synthetic(entries: &[(&str, Option<&str>)]) -> Vec<LogLine> {
        entries
            .iter()
            .map(|(msg, object)| {
                let object = object.map_or(String::new(), |name| {
                    format!(r#","object":"{name}","objectType":"*local.Object""#)
                });
                LogLine::parse(&format!(r#"{{"level":"info","msg":{msg:?}{object}}}"#))
            })
            .collect()
    }

    #[test]
    fn a_file_deleted_on_the_remote_and_changed_locally_is_copied_back_as_an_upload() {
        let log = synthetic(&[
            ("- Path1  File changed: size (larger)   - f.txt", None),
            ("- Path2  File was deleted   - f.txt", None),
            ("- Path1  Do queued copies to   - Path2", None),
            ("Copied (new)", Some("f.txt")),
            ("Bisync successful", None),
        ]);

        let summary = summarize(&log);

        assert_eq!(summary.changes, [change(ChangeKind::Upload, "f.txt")]);
    }

    #[test]
    fn a_file_deleted_locally_and_changed_on_the_remote_is_copied_back_as_a_download() {
        let log = synthetic(&[
            ("- Path1  File was deleted   - f.txt", None),
            ("- Path2  File changed: size (larger)   - f.txt", None),
            ("- Path2  Do queued copies to   - Path1", None),
            ("Copied (new)", Some("f.txt")),
            ("Bisync successful", None),
        ]);

        let summary = summarize(&log);

        assert_eq!(summary.changes, [change(ChangeKind::Download, "f.txt")]);
    }

    #[test]
    fn a_repeated_event_for_the_same_file_is_listed_once() {
        let log = synthetic(&[
            ("- Path1  Do queued copies to   - Path2", None),
            ("Copied (new)", Some("a.txt")),
            ("Copied (new)", Some("a.txt")),
            ("Bisync successful", None),
        ]);

        let summary = summarize(&log);

        assert_eq!(summary.changes, [change(ChangeKind::Upload, "a.txt")]);
        assert_eq!(summary.uploaded, Some(1));
    }

    #[test]
    fn summarize_of_an_empty_log_has_no_counters() {
        assert_eq!(summarize(&[]), RunSummary::default());
    }

    #[test]
    fn last_stats_of_a_finished_bulk_run_reads_the_final_totals() {
        let progress = last_stats(&fixture("stats")).unwrap();

        assert_eq!(progress.transfers, 200);
        assert_eq!(progress.total_transfers, 200);
        assert_eq!(progress.bytes, 13_107_200);
        assert_eq!(progress.total_bytes, 13_107_200);
        assert_eq!(progress.eta_seconds, Some(0));
        assert_eq!(progress.current, None);
        assert!((progress.speed - 2_151_756.198_941_915_8).abs() < 1.0);
    }

    #[test]
    fn last_stats_in_the_middle_of_a_transfer_reports_the_current_file_and_no_eta_yet() {
        let lines = fixture("stats");
        let first = lines
            .iter()
            .position(|line| line.progress.is_some())
            .unwrap();

        let progress = last_stats(&lines[..=first]).unwrap();

        assert_eq!(progress.transfers, 32);
        assert_eq!(progress.total_transfers, 200);
        assert_eq!(progress.bytes, 2_228_224);
        assert_eq!(progress.total_bytes, 13_107_200);
        assert_eq!(progress.eta_seconds, None);
        assert_eq!(progress.current.as_deref(), Some("bulk-127.bin"));
        assert!(progress.speed.abs() < f64::EPSILON);
    }

    #[test]
    fn last_stats_of_a_log_without_stats_lines_is_none() {
        let lines = fixture("upload");

        assert_eq!(last_stats(&lines[..5]), None);
    }
}
