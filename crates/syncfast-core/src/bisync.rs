//! The contract with `rclone bisync`: the fixed argument list and the
//! classification of how a run ended. See ADR 0002.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use jiff::Zoned;
use serde::{Deserialize, Serialize};

use crate::behavior::{Bandwidth, Behavior, ConflictLoser, GoogleDocs};
use crate::bisync_log::LogLine;
use crate::config::Pair;
use crate::paths::Paths;

const MAX_LOCK: &str = "2m";
const STATS_INTERVAL: &str = "2s";
pub const STAMP_FORMAT: &str = "%Y%m%d-%H%M%S";
const AUTH_PATTERNS: [&str; 4] = [
    "invalid_grant",
    "couldn't fetch token",
    "oauth2: cannot fetch token",
    "token expired",
];
const ACCOUNT_PATTERNS: [&str; 4] = [
    "service_disabled",
    "accessnotconfigured",
    "has not been used in project",
    "unauthorized_client",
];
const QUOTA_PATTERNS: [&str; 2] = ["storagequotaexceeded", "storage quota has been exceeded"];
const PERMISSION_PATTERNS: [&str; 3] = [
    "insufficientfilepermissions",
    "insufficientpermissions",
    "appnotauthorizedtofile",
];
const REMOTE_MISSING_PATTERNS: [&str; 1] = ["directory not found"];
const LOCAL_EMPTY_PATTERNS: [&str; 2] =
    ["empty current path1 listing", "empty current path2 listing"];

/// A failure that retrying does not fix: the user has to act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fault {
    /// The sign-in was lost or revoked.
    Auth,
    /// The account or its Google Cloud project refuses the app (API off,
    /// client not allowed).
    Account,
    Quota,
    Permission,
    RemoteMissing,
    /// rclone refuses to sync from a side that is now empty, usually a
    /// drive that is not mounted.
    LocalEmpty,
    /// A one-way sync whose source on the account holds no file; it never
    /// reaches rclone, so no log line names it.
    RemoteEmpty,
    /// The backup key is not in the key storage, or the storage cannot be
    /// opened without a person.
    KeyMissing,
    /// The key in the key storage does not open the backup.
    WrongKey,
    /// The folder that should hold the backup has no backup in it.
    RepositoryMissing,
    /// A folder to back up is gone or is not a folder.
    SourceMissing,
    /// A folder to back up holds nothing, usually a drive that is not mounted.
    SourceEmpty,
    /// The local destination of a backup is not there.
    DestinationMissing,
    /// `restic check` found damage that a retry does not fix.
    RepoDamaged,
}

/// The fault an rclone error line names, if any. Case-insensitive.
#[must_use]
pub fn fault_of(message: &str) -> Option<Fault> {
    let message = message.to_lowercase();
    let has = |patterns: &[&str]| patterns.iter().any(|pattern| message.contains(pattern));
    if has(&AUTH_PATTERNS) {
        Some(Fault::Auth)
    } else if has(&ACCOUNT_PATTERNS) {
        Some(Fault::Account)
    } else if has(&QUOTA_PATTERNS) {
        Some(Fault::Quota)
    } else if has(&PERMISSION_PATTERNS) {
        Some(Fault::Permission)
    } else if has(&REMOTE_MISSING_PATTERNS) {
        Some(Fault::RemoteMissing)
    } else if has(&LOCAL_EMPTY_PATTERNS) {
        Some(Fault::LocalEmpty)
    } else {
        None
    }
}

const TOO_MANY_DELETES: &str = "too many deletes";
const ALL_FILES_CHANGED: &str = "Safety abort: all files were changed";
const MUST_RESYNC: &str = "Must run --resync to recover";
const EMPTY_PRIOR_LISTINGS: [&str; 2] = ["Empty prior Path1 listing", "Empty prior Path2 listing"];
const VANISHED_DIR_PATTERNS: [&str; 2] = [
    "error reading source directory: directory not found",
    "error reading destination directory: directory not found",
];
/// Renames become moves and do not count toward `--max-delete`. Never
/// combined with `--slow-hash-sync-only`, which rclone rejects with a hash
/// strategy (ADR 0002).
const RENAME_ARGS: [&str; 4] = [
    "--track-renames",
    "--track-renames-strategy",
    "hash,modtime",
    "--max-delete-renames-aware",
];
/// Deletions on Drive go to its trash even when `rclone.conf` turns it off:
/// Recently Deleted and the review that confirms itself count on it.
pub(crate) const USE_TRASH: &str = "--drive-use-trash";
pub(crate) const FAILED_TO_BISYNC: &str = "Failed to bisync";
const EXIT_NOT_CRITICAL: i32 = 1;
pub(crate) const EXIT_USAGE: i32 = 2;
pub(crate) const EXIT_CRITICAL: i32 = 7;

/// What the user confirmed in the review: a first sync or a resync after an
/// error or a filter change (`Resync`), or going ahead with a mass deletion
/// (`Force`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewKind {
    Resync,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoundMode {
    Normal,
    Resync,
    Force,
    /// Nothing is changed; the log goes to [`log_file`]'s preview path.
    DryRun(ReviewKind),
}

pub(crate) fn stamp(now: &Zoned) -> String {
    now.strftime(STAMP_FORMAT).to_string()
}

/// Where rclone writes this run's JSON log. A dry run writes next to the
/// pair's state (`preview-<stamp>.jsonl`), outside `runs/`, so previews never
/// show up in Activity or count toward the 50 kept runs; whoever starts the
/// preview reads that file and deletes it.
#[must_use]
pub fn log_file(paths: &Paths, pair_id: &str, mode: RoundMode, now: &Zoned) -> PathBuf {
    let stamp = stamp(now);
    match mode {
        RoundMode::DryRun(_) => paths
            .pair_state_dir(pair_id)
            .join(format!("preview-{stamp}.jsonl")),
        RoundMode::Normal | RoundMode::Resync | RoundMode::Force => {
            paths.runs_dir(pair_id).join(format!("{stamp}.jsonl"))
        }
    }
}

pub(crate) fn dir_arg(path: &Path) -> OsString {
    let mut arg = path.as_os_str().to_owned();
    arg.push("/");
    arg
}

/// Arguments for `rclone` (without the binary), in a fixed order. Every
/// tunable comes from `behavior`; the rest is constant. Local is Path1 and
/// the remote is Path2.
#[must_use]
pub fn bisync_args(
    pair: &Pair,
    behavior: &Behavior,
    paths: &Paths,
    mode: RoundMode,
    now: &Zoned,
) -> Vec<OsString> {
    let id = pair.id.as_str();
    let mut args: Vec<OsString> = vec![
        "bisync".into(),
        pair.local.clone().into_os_string(),
        pair.remote.clone().into(),
        "--filters-file".into(),
        paths.filters_file(id).into_os_string(),
        "--workdir".into(),
        dir_arg(&paths.bisync_workdir(id)),
        "--resilient".into(),
        "--recover".into(),
        "--max-lock".into(),
        MAX_LOCK.into(),
        "--conflict-resolve".into(),
        behavior.conflict_resolve.rclone_value().into(),
        "--conflict-loser".into(),
        behavior.conflict_loser.rclone_value().into(),
    ];
    if behavior.conflict_loser == ConflictLoser::Pathname {
        args.extend(["--conflict-suffix".into(), "path".into()]);
    }
    args.extend([
        "--max-delete".into(),
        behavior.max_delete.to_string().into(),
    ]);
    if tracks_renames(mode) {
        args.extend(RENAME_ARGS.map(OsString::from));
    }
    if behavior.empty_dirs {
        args.push("--create-empty-src-dirs".into());
    }
    if behavior.google_docs == GoogleDocs::Skip {
        args.push("--drive-skip-gdocs".into());
    }
    if behavior.versions {
        let backup = paths.versions_dir(id).join(stamp(now));
        args.extend(["--backup-dir1".into(), dir_arg(&backup)]);
    }
    args.extend(bwlimit_args(behavior));
    args.extend([
        USE_TRASH.into(),
        "--stats".into(),
        STATS_INTERVAL.into(),
        "--stats-log-level".into(),
        "NOTICE".into(),
        "--use-json-log".into(),
        "--log-file".into(),
        log_file(paths, id, mode, now).into_os_string(),
        "-v".into(),
    ]);
    args.extend(mode_args(behavior, mode));
    args
}

/// `--bwlimit <UP>:<DOWN>`, both sides always written (rclone rejects an
/// empty one); nothing when both are unlimited.
#[must_use]
pub fn bwlimit_args(behavior: &Behavior) -> Vec<OsString> {
    if behavior.limit_upload == Bandwidth::Off && behavior.limit_download == Bandwidth::Off {
        return Vec::new();
    }
    vec![
        "--bwlimit".into(),
        format!(
            "{}:{}",
            behavior.limit_upload.format(),
            behavior.limit_download.format()
        )
        .into(),
    ]
}

fn mode_args(behavior: &Behavior, mode: RoundMode) -> Vec<OsString> {
    let resync = || -> Vec<OsString> {
        vec![
            "--resync".into(),
            "--resync-mode".into(),
            behavior.resync_mode.rclone_value().into(),
        ]
    };
    match mode {
        RoundMode::Normal => Vec::new(),
        RoundMode::Resync => resync(),
        RoundMode::Force => vec!["--force".into()],
        RoundMode::DryRun(kind) => {
            let mut args = match kind {
                ReviewKind::Resync => resync(),
                ReviewKind::Force => vec!["--force".into()],
            };
            args.push("--dry-run".into());
            args
        }
    }
}

/// A resync is a copy, where rclone ignores `--track-renames` with an error
/// line; every other mode applies renames as server-side moves.
fn tracks_renames(mode: RoundMode) -> bool {
    matches!(
        mode,
        RoundMode::Normal | RoundMode::Force | RoundMode::DryRun(ReviewKind::Force)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedsReviewCause {
    CriticalError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    NeedsReview(NeedsReviewCause),
    TooManyDeletes,
    /// rclone's safety abort when every file of a side changed at once;
    /// like [`Outcome::TooManyDeletes`], `--force` lets it through.
    AllFilesChanged,
    AuthError,
    /// A failure that retrying does not fix and the user can act on. The
    /// next round still runs: it ends the attention when it succeeds.
    NeedsAttention {
        fault: Fault,
        /// The failure message that named the fault.
        detail: String,
    },
    /// rclone rejected the arguments: a syncfast bug.
    ConfigError,
    Transient,
}

/// True when rclone renamed a listing to `.lst-err` in this run: the
/// `.lst-err` has no `.lst` beside it. The rename keeps the mtime, so a
/// stale `.lst-err` next to a valid listing is told apart only this way. A
/// workdir that cannot be read (other than being absent) counts as lost:
/// assuming the listings are fine is the unsafe direction.
fn listings_lost(workdir: &Path) -> bool {
    let names: HashSet<String> = match std::fs::read_dir(workdir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(err) => return err.kind() != std::io::ErrorKind::NotFound,
    };
    names.iter().any(|name| {
        name.strip_suffix(".lst-err")
            .is_some_and(|base| !names.contains(&format!("{base}.lst")))
    })
}

/// A subfolder that disappeared while rclone was listing it (renamed or
/// deleted mid-run). The next round lists the new state.
fn folder_vanished_mid_listing(lines: &[LogLine]) -> bool {
    lines.iter().any(|line| {
        line.level.is_failure()
            && VANISHED_DIR_PATTERNS
                .iter()
                .any(|pattern| line.msg.starts_with(pattern))
            && line
                .object
                .as_deref()
                .is_some_and(|object| !object.is_empty())
    })
}

/// Lines that can say why a run failed: errors, anything that was not JSON,
/// and the closing `Failed to bisync` notice. Info and dry-run notices also
/// quote file names, which must not be mistaken for error text.
pub(crate) fn failure_msgs(lines: &[LogLine]) -> impl Iterator<Item = &str> {
    lines
        .iter()
        .filter(|line| line.level.is_failure() || line.msg.starts_with(FAILED_TO_BISYNC))
        .map(|line| line.msg.as_str())
}

/// How a run that exited with `exit_code` ended. The first test that
/// matches wins; `workdir` is the pair's bisync work directory.
#[must_use]
pub fn classify(exit_code: i32, lines: &[LogLine], workdir: &Path) -> Outcome {
    if exit_code == 0 {
        return Outcome::Ok;
    }
    if listings_lost(workdir) {
        return Outcome::NeedsReview(NeedsReviewCause::CriticalError);
    }
    if failure_msgs(lines).any(|msg| fault_of(msg) == Some(Fault::Auth)) {
        return Outcome::AuthError;
    }
    if exit_code == EXIT_NOT_CRITICAL
        && failure_msgs(lines).any(|msg| msg.contains(TOO_MANY_DELETES))
    {
        return Outcome::TooManyDeletes;
    }
    if exit_code == EXIT_NOT_CRITICAL
        && failure_msgs(lines).any(|msg| msg.contains(ALL_FILES_CHANGED))
    {
        return Outcome::AllFilesChanged;
    }
    if folder_vanished_mid_listing(lines) {
        return Outcome::Transient;
    }
    if let Some((fault, msg)) = failure_msgs(lines).find_map(|msg| fault_of(msg).map(|f| (f, msg)))
    {
        return Outcome::NeedsAttention {
            fault,
            detail: msg.to_owned(),
        };
    }
    if exit_code == EXIT_CRITICAL && failure_msgs(lines).any(|msg| msg.contains(MUST_RESYNC)) {
        return Outcome::NeedsReview(NeedsReviewCause::CriticalError);
    }
    if exit_code == EXIT_USAGE {
        return Outcome::ConfigError;
    }
    Outcome::Transient
}

/// True when a normal round stopped because the listings it keeps from the
/// last sync are empty (both sides were empty then). rclone refuses to
/// sync from that state, so the caller retries once as a resync.
#[must_use]
pub fn is_empty_prior_listing(exit_code: i32, lines: &[LogLine]) -> bool {
    exit_code == EXIT_CRITICAL
        && failure_msgs(lines).any(|msg| {
            EMPTY_PRIOR_LISTINGS
                .iter()
                .any(|pattern| msg.starts_with(pattern))
        })
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::{self, TimeZone};

    use super::*;
    use crate::behavior::{ConflictResolve, ResyncMode};
    use crate::bisync_log::fixtures::{exit_code, fixture, fixture_path};
    use crate::bisync_log::parse_log;

    fn now() -> Zoned {
        date(2026, 1, 2)
            .at(15, 4, 5, 0)
            .to_zoned(TimeZone::fixed(tz::offset(-3)))
            .unwrap()
    }

    fn paths() -> Paths {
        Paths::new(
            "/home/ada".into(),
            "/c".into(),
            "/s".into(),
            "/d".into(),
            "/r".into(),
        )
    }

    fn pair() -> Pair {
        Pair {
            id: "drive".into(),
            name: "Drive".into(),
            local: "/home/ada/Drive".into(),
            remote: "gdrive:Notes".into(),
            enabled: true,
            presets: vec![],
            filters: vec![],
            overrides: crate::settings::Overrides::default(),
        }
    }

    fn args(behavior: &Behavior, mode: RoundMode) -> Vec<String> {
        bisync_args(&pair(), behavior, &paths(), mode, &now())
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    const COMMON: [&str; 29] = [
        "bisync",
        "/home/ada/Drive",
        "gdrive:Notes",
        "--filters-file",
        "/s/syncfast/filters/drive.txt",
        "--workdir",
        "/s/syncfast/bisync/drive/",
        "--resilient",
        "--recover",
        "--max-lock",
        "2m",
        "--conflict-resolve",
        "newer",
        "--conflict-loser",
        "num",
        "--max-delete",
        "50",
        "--create-empty-src-dirs",
        "--drive-skip-gdocs",
        "--backup-dir1",
        "/d/syncfast/versions/drive/20260102-150405/",
        "--drive-use-trash",
        "--stats",
        "2s",
        "--stats-log-level",
        "NOTICE",
        "--use-json-log",
        "--log-file",
        "/s/syncfast/pairs/drive/runs/20260102-150405.jsonl",
    ];

    const RENAMES: [&str; 4] = [
        "--track-renames",
        "--track-renames-strategy",
        "hash,modtime",
        "--max-delete-renames-aware",
    ];

    fn expected_resync(log: &str, mode_flags: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = COMMON.iter().map(|arg| (*arg).to_owned()).collect();
        *args.last_mut().unwrap() = log.to_owned();
        args.push("-v".to_owned());
        args.extend(mode_flags.iter().map(|arg| (*arg).to_owned()));
        args
    }

    fn expected(log: &str, mode_flags: &[&str]) -> Vec<String> {
        let mut args = expected_resync(log, mode_flags);
        let after_max_delete = args.iter().position(|arg| arg == "--max-delete").unwrap() + 2;
        args.splice(
            after_max_delete..after_max_delete,
            RENAMES.map(str::to_owned),
        );
        args
    }

    const RUN_LOG: &str = "/s/syncfast/pairs/drive/runs/20260102-150405.jsonl";
    const PREVIEW_LOG: &str = "/s/syncfast/pairs/drive/preview-20260102-150405.jsonl";

    #[test]
    fn normal_mode_uses_only_the_fixed_arguments() {
        let args = args(&Behavior::default(), RoundMode::Normal);

        assert_eq!(args, expected(RUN_LOG, &[]));
    }

    fn limits(upload: Bandwidth, download: Bandwidth) -> Behavior {
        Behavior {
            limit_upload: upload,
            limit_download: download,
            ..Behavior::default()
        }
    }

    fn bwlimit(behavior: &Behavior) -> Option<String> {
        let args = args(behavior, RoundMode::Normal);
        let at = args.iter().position(|arg| arg == "--bwlimit")?;
        Some(args[at + 1].clone())
    }

    #[test]
    fn bwlimit_names_both_directions_upload_first() {
        let kib = Bandwidth::KiBPerSec;

        assert_eq!(
            bwlimit(&limits(kib(2048), Bandwidth::Off)).as_deref(),
            Some("2M:off")
        );
        assert_eq!(
            bwlimit(&limits(Bandwidth::Off, kib(512))).as_deref(),
            Some("off:512K")
        );
        assert_eq!(
            bwlimit(&limits(kib(1024), kib(2048))).as_deref(),
            Some("1M:2M")
        );
    }

    #[test]
    fn bwlimit_is_left_out_when_both_directions_are_unlimited() {
        let behavior = limits(Bandwidth::Off, Bandwidth::Off);

        assert_eq!(bwlimit(&behavior), None);
        assert_eq!(args(&behavior, RoundMode::Normal), expected(RUN_LOG, &[]));
    }

    #[test]
    fn bwlimit_goes_to_every_mode_of_a_round() {
        let behavior = limits(Bandwidth::KiBPerSec(512), Bandwidth::Off);

        for mode in [RoundMode::Normal, RoundMode::Resync, RoundMode::Force] {
            assert!(
                args(&behavior, mode)
                    .windows(2)
                    .any(|pair| pair == ["--bwlimit", "512K:off"]),
                "{mode:?}"
            );
        }
    }

    #[test]
    fn resync_mode_adds_resync_with_the_newer_policy() {
        let args = args(&Behavior::default(), RoundMode::Resync);

        assert_eq!(
            args,
            expected_resync(RUN_LOG, &["--resync", "--resync-mode", "newer"])
        );
    }

    #[test]
    fn force_mode_adds_force() {
        let args = args(&Behavior::default(), RoundMode::Force);

        assert_eq!(args, expected(RUN_LOG, &["--force"]));
    }

    #[test]
    fn resync_dry_run_adds_dry_run_and_logs_to_the_preview_file() {
        let args = args(&Behavior::default(), RoundMode::DryRun(ReviewKind::Resync));

        assert_eq!(
            args,
            expected_resync(
                PREVIEW_LOG,
                &["--resync", "--resync-mode", "newer", "--dry-run"]
            )
        );
    }

    #[test]
    fn force_dry_run_adds_force_and_dry_run_and_logs_to_the_preview_file() {
        let args = args(&Behavior::default(), RoundMode::DryRun(ReviewKind::Force));

        assert_eq!(args, expected(PREVIEW_LOG, &["--force", "--dry-run"]));
    }

    #[test]
    fn log_file_is_the_one_in_the_arguments() {
        let normal = log_file(&paths(), "drive", RoundMode::Normal, &now());
        let preview = log_file(
            &paths(),
            "drive",
            RoundMode::DryRun(ReviewKind::Force),
            &now(),
        );

        assert_eq!(normal, Path::new(RUN_LOG));
        assert_eq!(preview, Path::new(PREVIEW_LOG));
    }

    #[test]
    fn every_tunable_flag_follows_the_behavior() {
        let behavior = Behavior {
            conflict_resolve: ConflictResolve::Local,
            conflict_loser: ConflictLoser::Pathname,
            max_delete: 20,
            resync_mode: ResyncMode::Remote,
            ..Behavior::default()
        };

        let args = args(&behavior, RoundMode::Resync);

        let value_of = |flag: &str| {
            let at = args.iter().position(|arg| arg == flag).unwrap();
            args[at + 1].clone()
        };
        assert_eq!(value_of("--conflict-resolve"), "path1");
        assert_eq!(value_of("--conflict-loser"), "pathname");
        assert_eq!(value_of("--max-delete"), "20");
        assert_eq!(value_of("--resync-mode"), "path2");
    }

    fn value_after(args: &[String], flag: &str) -> Option<String> {
        let at = args.iter().position(|arg| arg == flag)?;
        args.get(at + 1).cloned()
    }

    #[test]
    fn each_conflict_resolve_value_reaches_rclone_under_its_own_name() {
        for (resolve, flag) in [
            (ConflictResolve::None, "none"),
            (ConflictResolve::Newer, "newer"),
            (ConflictResolve::Older, "older"),
            (ConflictResolve::Larger, "larger"),
            (ConflictResolve::Smaller, "smaller"),
            (ConflictResolve::Local, "path1"),
            (ConflictResolve::Remote, "path2"),
        ] {
            let behavior = Behavior {
                conflict_resolve: resolve,
                ..Behavior::default()
            };

            let args = args(&behavior, RoundMode::Normal);

            assert_eq!(
                value_after(&args, "--conflict-resolve").as_deref(),
                Some(flag)
            );
        }
    }

    #[test]
    fn a_pathname_loser_adds_the_path_suffix_and_the_others_add_none() {
        for (loser, suffix) in [
            (ConflictLoser::Num, None),
            (ConflictLoser::Pathname, Some("path")),
            (ConflictLoser::Delete, None),
        ] {
            let behavior = Behavior {
                conflict_loser: loser,
                ..Behavior::default()
            };

            let args = args(&behavior, RoundMode::Normal);

            assert_eq!(
                value_after(&args, "--conflict-loser").as_deref(),
                Some(loser.rclone_value())
            );
            assert_eq!(value_after(&args, "--conflict-suffix").as_deref(), suffix);
        }
    }

    #[test]
    fn versions_false_passes_no_backup_dir() {
        let behavior = Behavior {
            versions: false,
            ..Behavior::default()
        };

        let args = args(&behavior, RoundMode::Normal);

        assert!(!args.iter().any(|arg| arg.starts_with("--backup-dir")));
    }

    #[test]
    fn the_retention_days_never_reach_rclone() {
        let behavior = Behavior {
            versions_keep_days: 7,
            ..Behavior::default()
        };

        assert_eq!(args(&behavior, RoundMode::Normal), expected(RUN_LOG, &[]));
    }

    #[test]
    fn switched_off_behaviors_drop_their_flags() {
        let behavior = Behavior {
            versions: false,
            empty_dirs: false,
            google_docs: GoogleDocs::Export,
            ..Behavior::default()
        };

        let args = args(&behavior, RoundMode::Normal);

        assert!(!args.contains(&"--backup-dir1".to_owned()));
        assert!(!args.contains(&"--create-empty-src-dirs".to_owned()));
        assert!(!args.contains(&"--drive-skip-gdocs".to_owned()));
    }

    fn workdir_with(names: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in names {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        dir
    }

    fn classify_fixture(name: &str, workdir: &Path) -> Outcome {
        classify(exit_code(name), &fixture(name), workdir)
    }

    fn lines(entries: &[(&str, &str)]) -> Vec<LogLine> {
        entries
            .iter()
            .map(|(level, msg)| LogLine::parse(&format!(r#"{{"level":"{level}","msg":{msg:?}}}"#)))
            .collect()
    }

    #[test]
    fn exit_zero_is_ok() {
        let workdir = workdir_with(&[]);

        assert_eq!(classify_fixture("upload", workdir.path()), Outcome::Ok);
    }

    #[test]
    fn every_conflict_fixture_is_an_ok_run() {
        let workdir = workdir_with(&[]);

        for name in [
            "conflict",
            "conflict_none",
            "conflict_pathname",
            "conflict_pathname_none",
            "conflict_delete",
            "conflict_local_wins",
        ] {
            assert_eq!(
                classify_fixture(name, workdir.path()),
                Outcome::Ok,
                "{name}"
            );
        }
    }

    #[test]
    fn exit_zero_is_ok_even_with_listings_marked_as_failed() {
        let workdir = workdir_with(&["a.path1.lst-err"]);

        assert_eq!(classify_fixture("upload", workdir.path()), Outcome::Ok);
    }

    #[test]
    fn lst_err_files_in_the_workdir_need_review_whatever_the_log_says() {
        let names: Vec<String> = std::fs::read_to_string(fixture_path("critical.lst-err"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert!(!names.is_empty());
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let workdir = workdir_with(&names);

        let outcome = classify_fixture("critical_retryable", workdir.path());

        assert_eq!(
            outcome,
            Outcome::NeedsReview(NeedsReviewCause::CriticalError)
        );
    }

    #[test]
    fn lst_err_wins_over_an_authentication_error() {
        let workdir = workdir_with(&["x.path1.lst-err"]);
        let log = lines(&[("error", "oauth2: cannot fetch token")]);

        assert_eq!(
            classify(7, &log, workdir.path()),
            Outcome::NeedsReview(NeedsReviewCause::CriticalError)
        );
    }

    #[test]
    fn each_authentication_pattern_is_an_auth_error_whatever_its_case() {
        let workdir = workdir_with(&[]);
        for text in [
            "Failed to create file system: invalid_grant",
            "Failed: Couldn't Fetch Token: x",
            "Get \"https://x\": oauth2: cannot fetch token: 400",
            "Token EXPIRED and refresh failed",
        ] {
            let log = lines(&[("error", text)]);

            assert_eq!(
                classify(1, &log, workdir.path()),
                Outcome::AuthError,
                "{text}"
            );
        }
    }

    #[test]
    fn authentication_text_in_the_closing_notice_is_an_auth_error() {
        let workdir = workdir_with(&[]);
        let log = lines(&[("notice", "Failed to bisync: invalid_grant")]);

        assert_eq!(classify(1, &log, workdir.path()), Outcome::AuthError);
    }

    #[test]
    fn a_file_name_that_looks_like_an_auth_error_in_an_info_line_is_ignored() {
        let workdir = workdir_with(&[]);
        let log = lines(&[("info", "- Path1  File is new  - invalid_grant.txt")]);

        assert_eq!(classify(5, &log, workdir.path()), Outcome::Transient);
    }

    #[test]
    fn auth_error_wins_over_too_many_deletes() {
        let workdir = workdir_with(&[]);
        let log = lines(&[
            ("error", "too many deletes (>50%, 3 of 4)"),
            ("error", "token expired"),
        ]);

        assert_eq!(classify(1, &log, workdir.path()), Outcome::AuthError);
    }

    #[test]
    fn backup_faults_serialize_with_snake_case_names() {
        let cases = [
            (Fault::KeyMissing, "key_missing"),
            (Fault::WrongKey, "wrong_key"),
            (Fault::RepositoryMissing, "repository_missing"),
            (Fault::SourceMissing, "source_missing"),
            (Fault::SourceEmpty, "source_empty"),
            (Fault::DestinationMissing, "destination_missing"),
            (Fault::RepoDamaged, "repo_damaged"),
        ];
        for (fault, name) in cases {
            let json = serde_json::to_string(&fault).unwrap();

            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(serde_json::from_str::<Fault>(&json).unwrap(), fault);
        }
    }

    #[test]
    fn fault_of_names_the_google_error_texts() {
        let cases = [
            (
                "Error 403: Google Drive API has not been used in project 1 before or it is disabled",
                Some(Fault::Account),
            ),
            (
                "googleapi: Error 403: The user's Drive storage quota has been exceeded., storageQuotaExceeded",
                Some(Fault::Quota),
            ),
            (
                "googleapi: Error 403: The user does not have sufficient permissions for this file., insufficientFilePermissions",
                Some(Fault::Permission),
            ),
            (
                "error listing: directory not found",
                Some(Fault::RemoteMissing),
            ),
            (
                "Empty current Path1 listing. Cannot sync to an empty directory: /x.lst-new",
                Some(Fault::LocalEmpty),
            ),
            (
                "Empty current Path2 listing. Cannot sync to an empty directory: /x.lst-new",
                Some(Fault::LocalEmpty),
            ),
            (
                "Failed to create file system: invalid_grant",
                Some(Fault::Auth),
            ),
            (
                "googleapi: Error 403: Rate Limit Exceeded, userRateLimitExceeded",
                None,
            ),
        ];

        for (text, fault) in cases {
            assert_eq!(fault_of(text), fault, "{text}");
        }
    }

    #[test]
    fn each_non_auth_fault_needs_attention_with_the_matching_message() {
        let workdir = workdir_with(&[]);
        for (text, fault) in [
            (
                "Error 403: Google Drive API has not been used in project 1 before or it is disabled",
                Fault::Account,
            ),
            ("storageQuotaExceeded", Fault::Quota),
            ("insufficientFilePermissions", Fault::Permission),
            ("directory not found", Fault::RemoteMissing),
            (
                "Empty current Path1 listing. Cannot sync to an empty directory: x",
                Fault::LocalEmpty,
            ),
        ] {
            let log = lines(&[
                ("info", "- Path1  File is new  - a.txt"),
                ("error", text),
                ("notice", "Failed to bisync: bisync aborted"),
            ]);

            assert_eq!(
                classify(1, &log, workdir.path()),
                Outcome::NeedsAttention {
                    fault,
                    detail: text.to_owned()
                },
                "{text}"
            );
        }
    }

    #[test]
    fn a_fault_in_the_closing_notice_needs_attention() {
        let workdir = workdir_with(&[]);
        let log = lines(&[("notice", "Failed to bisync: directory not found")]);

        assert_eq!(
            classify(7, &log, workdir.path()),
            Outcome::NeedsAttention {
                fault: Fault::RemoteMissing,
                detail: "Failed to bisync: directory not found".to_owned()
            }
        );
    }

    #[test]
    fn auth_wins_over_the_other_faults() {
        let workdir = workdir_with(&[]);
        let log = lines(&[
            ("error", "storageQuotaExceeded"),
            ("error", "invalid_grant"),
        ]);

        assert_eq!(classify(1, &log, workdir.path()), Outcome::AuthError);
    }

    #[test]
    fn too_many_deletes_and_all_files_changed_win_over_a_non_auth_fault() {
        let workdir = workdir_with(&[]);
        for (text, expected) in [
            ("too many deletes (>50%, 3 of 4)", Outcome::TooManyDeletes),
            (ALL_FILES_CHANGED, Outcome::AllFilesChanged),
        ] {
            let log = lines(&[("error", "storageQuotaExceeded"), ("error", text)]);

            assert_eq!(classify(1, &log, workdir.path()), expected, "{text}");
        }
    }

    #[test]
    fn a_rate_limit_error_is_transient() {
        let workdir = workdir_with(&[]);
        let log = lines(&[(
            "error",
            "Error 403: Rate Limit Exceeded, userRateLimitExceeded",
        )]);

        assert_eq!(classify(1, &log, workdir.path()), Outcome::Transient);
    }

    #[test]
    fn a_workdir_that_cannot_be_read_needs_review() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_folder = dir.path().join("file");
        std::fs::write(&not_a_folder, "").unwrap();

        let outcome = classify(5, &[], &not_a_folder);

        assert_eq!(
            outcome,
            Outcome::NeedsReview(NeedsReviewCause::CriticalError)
        );
    }

    #[test]
    fn exit_one_with_too_many_deletes_is_too_many_deletes() {
        let workdir = workdir_with(&[]);

        assert_eq!(
            classify_fixture("too_many_deletes", workdir.path()),
            Outcome::TooManyDeletes
        );
    }

    #[test]
    fn exit_one_with_the_all_files_changed_safety_abort_is_all_files_changed() {
        let workdir = workdir_with(&[]);

        assert_eq!(
            classify_fixture("all_files_changed", workdir.path()),
            Outcome::AllFilesChanged
        );
    }

    #[test]
    fn all_files_changed_text_with_another_exit_code_is_transient() {
        let workdir = workdir_with(&[]);

        assert_eq!(
            classify(5, &fixture("all_files_changed"), workdir.path()),
            Outcome::Transient
        );
    }

    #[test]
    fn too_many_deletes_text_with_another_exit_code_is_not_too_many_deletes() {
        let workdir = workdir_with(&[]);

        assert_eq!(
            classify(5, &fixture("too_many_deletes"), workdir.path()),
            Outcome::Transient
        );
    }

    #[test]
    fn exit_seven_with_must_run_resync_needs_review() {
        let workdir = workdir_with(&[]);

        assert_eq!(
            classify_fixture("critical", workdir.path()),
            Outcome::NeedsReview(NeedsReviewCause::CriticalError)
        );
    }

    #[test]
    fn exit_two_is_a_config_error() {
        let workdir = workdir_with(&[]);
        let log = lines(&[("error", "unknown flag: --nope")]);

        assert_eq!(classify(2, &log, workdir.path()), Outcome::ConfigError);
    }

    #[test]
    fn a_retryable_critical_error_without_listings_marked_failed_is_transient() {
        let workdir = workdir_with(&[]);

        assert_eq!(
            classify_fixture("critical_retryable", workdir.path()),
            Outcome::Transient
        );
    }

    #[test]
    fn stale_lst_err_beside_a_valid_listing_does_not_block() {
        let workdir = workdir_with(&["x.path1.lst", "x.path1.lst-err"]);

        assert_eq!(
            classify_fixture("critical_retryable", workdir.path()),
            Outcome::Transient
        );
    }

    #[test]
    fn a_folder_that_vanished_mid_listing_is_transient() {
        let workdir = workdir_with(&["x.path1.lst", "x.path2.lst"]);
        let log = parse_log(concat!(
            r#"{"level":"error","msg":"error reading source directory: directory not found","object":"estrutura-de-dados-ii/provas/p1","objectType":"string","source":"march/march.go:566"}"#,
            "\n",
            r#"{"level":"error","msg":"There were errors while building listings. Aborting as it is too dangerous to continue."}"#,
            "\n",
            r#"{"level":"error","msg":"Bisync critical error: march failed with 29 error(s): first error: directory not found"}"#,
            "\n",
            r#"{"level":"error","msg":"Bisync aborted. Error is retryable without --resync due to --resilient mode."}"#,
            "\n",
            r#"{"level":"notice","msg":"Failed to bisync with 30 errors: last error was: bisync aborted"}"#,
        ));

        assert_eq!(classify(7, &log, workdir.path()), Outcome::Transient);
    }

    #[test]
    fn a_subfolder_that_cannot_be_read_for_lack_of_permission_still_needs_attention() {
        let workdir = workdir_with(&["x.path1.lst", "x.path2.lst"]);
        let log = parse_log(concat!(
            r#"{"level":"error","msg":"error reading source directory: googleapi: Error 403: The user does not have sufficient permissions for this file., insufficientFilePermissions","object":"estrutura-de-dados-ii/provas/p1","objectType":"string"}"#,
            "\n",
            r#"{"level":"notice","msg":"Failed to bisync with 2 errors: last error was: bisync aborted"}"#,
        ));

        assert!(matches!(
            classify(7, &log, workdir.path()),
            Outcome::NeedsAttention {
                fault: Fault::Permission,
                ..
            }
        ));
    }

    #[test]
    fn a_changed_filters_file_is_transient_because_the_run_decision_handles_it_first() {
        let workdir = workdir_with(&[]);

        assert_eq!(
            classify_fixture("filters_changed", workdir.path()),
            Outcome::Transient
        );
    }

    #[test]
    fn any_other_failure_is_transient() {
        let workdir = workdir_with(&[]);
        let log = lines(&[("error", "connection reset by peer")]);

        assert_eq!(classify(1, &log, workdir.path()), Outcome::Transient);
    }

    #[test]
    fn a_missing_workdir_is_not_a_failed_listing() {
        let dir = tempfile::tempdir().unwrap();

        let outcome = classify(5, &[], &dir.path().join("absent"));

        assert_eq!(outcome, Outcome::Transient);
    }

    #[test]
    fn the_empty_prior_listing_fixture_is_recognised() {
        let log = fixture("empty_prior_listing");

        assert!(is_empty_prior_listing(
            exit_code("empty_prior_listing"),
            &log
        ));
    }

    #[test]
    fn an_empty_prior_path2_listing_is_recognised() {
        let log = lines(&[(
            "error",
            "Empty prior Path2 listing. Cannot sync to an empty directory: x",
        )]);

        assert!(is_empty_prior_listing(7, &log));
    }

    #[test]
    fn other_critical_failures_are_not_an_empty_prior_listing() {
        let retryable = fixture("critical_retryable");

        assert!(!is_empty_prior_listing(
            exit_code("critical_retryable"),
            &retryable
        ));
    }

    #[test]
    fn an_empty_prior_listing_message_with_another_exit_code_is_not_recognised() {
        let log = lines(&[(
            "error",
            "Empty prior Path1 listing. Cannot sync to an empty directory: x",
        )]);

        assert!(!is_empty_prior_listing(1, &log));
    }

    #[test]
    fn a_file_named_like_the_message_in_an_info_line_is_not_recognised() {
        let log = lines(&[("info", "Empty prior Path1 listing.txt: Copied")]);

        assert!(!is_empty_prior_listing(7, &log));
    }
}
