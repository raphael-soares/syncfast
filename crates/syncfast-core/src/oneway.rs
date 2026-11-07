//! The contract with `rclone copy` and `rclone sync`: the fixed argument
//! list of the one-way modes and the reading of how a run ended. See ADR
//! 0008. The report of what a run changed comes from `--combined`, whose
//! format is pinned by the `oneway_*` fixtures.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;

use jiff::Zoned;

use crate::behavior::{Behavior, GoogleDocs, SyncMode};
use crate::bisync::{
    EXIT_CRITICAL, EXIT_USAGE, Fault, Outcome, RoundMode, bwlimit_args, dir_arg, failure_msgs,
    fault_of, log_file, stamp,
};
use crate::bisync_log::{Change, ChangeKind, LogLine, RunSummary};
use crate::config::{Pair, split_remote};
use crate::paths::Paths;
use crate::rclone::{Rclone, RcloneError};
use crate::versions;

/// The folder on the account where `mirror_up` keeps what it replaces.
pub const REMOTE_VERSIONS_DIR: &str = ".syncfast-versions";

const STATS_INTERVAL: &str = "2s";
const TOO_MANY_DELETES: &str = "--max-delete threshold reached";
const FATAL_DELETE: &str = "Got fatal error on delete";

/// `<account>:.syncfast-versions/<id>`: where the remote versions of a
/// `mirror_up` sync live. `None` for a remote without an account.
#[must_use]
pub fn remote_versions_dir(remote: &str, id: &str) -> Option<String> {
    let (account, _) = split_remote(remote)?;
    Some(format!("{account}:{REMOTE_VERSIONS_DIR}/{id}"))
}

/// Where `--combined` writes the report of the run: next to its log, with
/// the same stamp.
#[must_use]
pub fn changes_file(paths: &Paths, pair_id: &str, mode: RoundMode, now: &Zoned) -> PathBuf {
    log_file(paths, pair_id, mode, now).with_extension("changes")
}

/// How many files a `sync` may delete before rclone stops: `max_delete`
/// percent of what the destination held after the last good round.
#[must_use]
pub fn max_delete_limit(dest_files: u64, percent: u8) -> u64 {
    dest_files.saturating_mul(u64::from(percent)) / 100
}

/// Arguments for `rclone` (without the binary), in a fixed order. The pair's
/// local folder is the source of `upload` and `mirror_up`, and the
/// destination of the other two. `dest_files` is the count the last good
/// round left in the destination and only matters for the mirrors.
#[must_use]
pub fn oneway_args(
    pair: &Pair,
    behavior: &Behavior,
    paths: &Paths,
    mode: RoundMode,
    dest_files: u64,
    now: &Zoned,
) -> Vec<OsString> {
    let id = pair.id.as_str();
    let local = pair.local.clone().into_os_string();
    let remote = OsString::from(&pair.remote);
    let (source, destination) = if behavior.mode.local_is_source() {
        (local, remote)
    } else {
        (remote, local)
    };
    let verb = if behavior.mode.is_mirror() {
        "sync"
    } else {
        "copy"
    };
    let mut args: Vec<OsString> = vec![
        verb.into(),
        source,
        destination,
        "--filter-from".into(),
        paths.filters_file(id).into_os_string(),
    ];
    if behavior.mode.is_mirror() && mode == RoundMode::Normal {
        args.extend([
            "--max-delete".into(),
            max_delete_limit(dest_files, behavior.max_delete)
                .to_string()
                .into(),
        ]);
    }
    if behavior.empty_dirs {
        args.push("--create-empty-src-dirs".into());
    }
    if behavior.google_docs == GoogleDocs::Skip {
        args.push("--drive-skip-gdocs".into());
    }
    if let Some(backup) = backup_dir(pair, behavior, paths, now) {
        args.extend(["--backup-dir".into(), backup]);
    }
    args.extend([
        "--combined".into(),
        changes_file(paths, id, mode, now).into_os_string(),
    ]);
    args.extend(bwlimit_args(behavior));
    args.extend([
        "--retries".into(),
        "1".into(),
        "--stats".into(),
        STATS_INTERVAL.into(),
        "--stats-log-level".into(),
        "NOTICE".into(),
        "--use-json-log".into(),
        "--log-file".into(),
        log_file(paths, id, mode, now).into_os_string(),
        "-v".into(),
    ]);
    if matches!(mode, RoundMode::DryRun(_)) {
        args.push("--dry-run".into());
    }
    args
}

/// Where what the round replaces goes. Nothing for `upload`: it replaces
/// files on the account, whose own revisions are the history there.
fn backup_dir(pair: &Pair, behavior: &Behavior, paths: &Paths, now: &Zoned) -> Option<OsString> {
    if !behavior.versions {
        return None;
    }
    match behavior.mode {
        SyncMode::Upload => None,
        SyncMode::MirrorUp => remote_versions_dir(&pair.remote, &pair.id)
            .map(|dir| format!("{dir}/{}", stamp(now)).into()),
        SyncMode::Bisync | SyncMode::Download | SyncMode::MirrorDown => {
            Some(dir_arg(&paths.versions_dir(&pair.id).join(stamp(now))))
        }
    }
}

/// How a one-way run that exited with `exit_code` ended. The first test
/// that matches wins. There is nothing to review besides too many deletes:
/// no listings to lose and no all-files-changed abort.
#[must_use]
pub fn classify_oneway(exit_code: i32, lines: &[LogLine]) -> Outcome {
    if exit_code == 0 {
        return Outcome::Ok;
    }
    if failure_msgs(lines).any(|msg| fault_of(msg) == Some(Fault::Auth)) {
        return Outcome::AuthError;
    }
    if exit_code == EXIT_CRITICAL
        && failure_msgs(lines)
            .any(|msg| msg.contains(TOO_MANY_DELETES) || msg.contains(FATAL_DELETE))
    {
        return Outcome::TooManyDeletes;
    }
    if let Some((fault, msg)) = failure_msgs(lines).find_map(|msg| fault_of(msg).map(|f| (f, msg)))
    {
        return Outcome::NeedsAttention {
            fault,
            detail: msg.to_owned(),
        };
    }
    if exit_code == EXIT_USAGE {
        return Outcome::ConfigError;
    }
    Outcome::Transient
}

/// One line of the `--combined` report: a symbol, a space and the path.
fn entries(changes: &str) -> impl Iterator<Item = (char, &str)> {
    changes.lines().filter_map(|line| {
        let mut chars = line.chars();
        let symbol = chars.next()?;
        let path = chars.as_str().strip_prefix(' ')?;
        Some((symbol, path))
    })
}

/// The counters and the changes of a run, from its `--combined` report. The
/// counters are known only for a run that finished (`exit_code` 0). A file
/// rclone also marked as failed (`!`) did not change, whatever else the
/// report says about it. What `copy` would remove from the destination is
/// not a change of a `copy` mode.
#[must_use]
pub fn summarize_oneway(changes: &str, mode: SyncMode, exit_code: i32) -> RunSummary {
    let failed: HashSet<&str> = entries(changes)
        .filter(|(symbol, _)| *symbol == '!')
        .map(|(_, path)| path)
        .collect();
    let (new, replaced, removed) = match mode {
        SyncMode::Upload => (ChangeKind::Upload, ChangeKind::OverwriteRemote, None),
        SyncMode::MirrorUp => (
            ChangeKind::Upload,
            ChangeKind::OverwriteRemote,
            Some(ChangeKind::DeleteRemote),
        ),
        SyncMode::Download => (ChangeKind::Download, ChangeKind::OverwriteLocal, None),
        SyncMode::MirrorDown => (
            ChangeKind::Download,
            ChangeKind::OverwriteLocal,
            Some(ChangeKind::DeleteLocal),
        ),
        SyncMode::Bisync => (ChangeKind::Other, ChangeKind::Other, None),
    };
    let changes: Vec<Change> = entries(changes)
        .filter(|(_, path)| !failed.contains(path))
        .filter_map(|(symbol, path)| {
            let kind = match symbol {
                '+' => new,
                '*' => replaced,
                '-' => removed?,
                _ => return None,
            };
            Some(Change {
                kind,
                path: path.to_owned(),
            })
        })
        .collect();
    let count = |kinds: &[ChangeKind]| {
        let found = changes.iter().filter(|change| kinds.contains(&change.kind));
        (exit_code == 0).then(|| u32::try_from(found.count()).unwrap_or(u32::MAX))
    };
    RunSummary {
        uploaded: count(&[ChangeKind::Upload, ChangeKind::OverwriteRemote]),
        downloaded: count(&[ChangeKind::Download, ChangeKind::OverwriteLocal]),
        deleted_local: count(&[ChangeKind::DeleteLocal]),
        deleted_remote: count(&[ChangeKind::DeleteRemote]),
        conflicts_new: None,
        changes,
    }
}

/// How many files the destination holds after a round that wrote this
/// report: what is there already, what was added and what was replaced.
#[must_use]
pub fn files_at_destination(changes: &str) -> u64 {
    let failed: HashSet<&str> = entries(changes)
        .filter(|(symbol, _)| *symbol == '!')
        .map(|(_, path)| path)
        .collect();
    let present = entries(changes)
        .filter(|(symbol, path)| matches!(symbol, '=' | '+' | '*') && !failed.contains(path))
        .count();
    u64::try_from(present).unwrap_or(u64::MAX)
}

/// Deletes the dated folders of the remote versions of `pair` that are older
/// than `keep_days`. A versions folder that does not exist yet is fine.
/// Returns how many folders were deleted.
///
/// # Errors
/// rclone cannot list the versions or delete one of them.
pub async fn prune_remote_versions(
    rclone: &Rclone,
    pair: &Pair,
    keep_days: u32,
    now: &Zoned,
) -> Result<usize, RcloneError> {
    let Some(root) = remote_versions_dir(&pair.remote, &pair.id) else {
        return Ok(0);
    };
    if rclone.stat(&root).await?.is_none() {
        return Ok(0);
    }
    let names = rclone.list_dirs(&root).await?;
    let old = versions::expired(&names, keep_days, now).map_err(|_| RcloneError::Failed {
        command: "purge",
        detail: "cannot work out which versions are old".to_owned(),
    })?;
    for name in &old {
        rclone.purge(&format!("{root}/{name}")).await?;
    }
    Ok(old.len())
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::{self, TimeZone};

    use super::*;
    use crate::behavior::Bandwidth;
    use crate::bisync::ReviewKind;
    use crate::bisync_log::fixtures::{exit_code, fixture, fixture_path};

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

    fn behavior(mode: SyncMode) -> Behavior {
        Behavior {
            mode,
            ..Behavior::default()
        }
    }

    fn args_with(behavior: &Behavior, mode: RoundMode, dest_files: u64) -> Vec<String> {
        oneway_args(&pair(), behavior, &paths(), mode, dest_files, &now())
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    fn args(behavior: &Behavior, mode: RoundMode) -> Vec<String> {
        args_with(behavior, mode, 200)
    }

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    const FILTERS: &str = "/s/syncfast/filters/drive.txt";
    const RUN_LOG: &str = "/s/syncfast/pairs/drive/runs/20260102-150405.jsonl";
    const RUN_CHANGES: &str = "/s/syncfast/pairs/drive/runs/20260102-150405.changes";
    const PREVIEW_LOG: &str = "/s/syncfast/pairs/drive/preview-20260102-150405.jsonl";
    const PREVIEW_CHANGES: &str = "/s/syncfast/pairs/drive/preview-20260102-150405.changes";
    const LOCAL_BACKUP: &str = "/d/syncfast/versions/drive/20260102-150405/";
    const REMOTE_BACKUP: &str = "gdrive:.syncfast-versions/drive/20260102-150405";

    fn expected(
        verb: &str,
        (source, destination): (&str, &str),
        before_combined: &[&str],
        (changes, log): (&str, &str),
        after_log: &[&str],
    ) -> Vec<String> {
        let mut args = strings(&[verb, source, destination, "--filter-from", FILTERS]);
        args.extend(strings(before_combined));
        args.extend(strings(&["--combined", changes]));
        args.extend(strings(&[
            "--retries",
            "1",
            "--stats",
            "2s",
            "--stats-log-level",
            "NOTICE",
            "--use-json-log",
            "--log-file",
            log,
            "-v",
        ]));
        args.extend(strings(after_log));
        args
    }

    const LOCAL: &str = "/home/ada/Drive";
    const REMOTE: &str = "gdrive:Notes";

    #[test]
    fn upload_copies_from_the_local_folder_with_no_cap_and_no_backup() {
        let args = args(&behavior(SyncMode::Upload), RoundMode::Normal);

        assert_eq!(
            args,
            expected(
                "copy",
                (LOCAL, REMOTE),
                &["--create-empty-src-dirs", "--drive-skip-gdocs"],
                (RUN_CHANGES, RUN_LOG),
                &[],
            )
        );
    }

    #[test]
    fn download_copies_from_the_account_and_backs_up_into_the_local_versions() {
        let args = args(&behavior(SyncMode::Download), RoundMode::Normal);

        assert_eq!(
            args,
            expected(
                "copy",
                (REMOTE, LOCAL),
                &[
                    "--create-empty-src-dirs",
                    "--drive-skip-gdocs",
                    "--backup-dir",
                    LOCAL_BACKUP,
                ],
                (RUN_CHANGES, RUN_LOG),
                &[],
            )
        );
    }

    #[test]
    fn mirror_up_syncs_to_the_account_capped_and_backs_up_into_its_versions_folder() {
        let args = args(&behavior(SyncMode::MirrorUp), RoundMode::Normal);

        assert_eq!(
            args,
            expected(
                "sync",
                (LOCAL, REMOTE),
                &[
                    "--max-delete",
                    "100",
                    "--create-empty-src-dirs",
                    "--drive-skip-gdocs",
                    "--backup-dir",
                    REMOTE_BACKUP,
                ],
                (RUN_CHANGES, RUN_LOG),
                &[],
            )
        );
    }

    #[test]
    fn mirror_down_syncs_into_the_local_folder_capped_and_backs_up_locally() {
        let args = args(&behavior(SyncMode::MirrorDown), RoundMode::Normal);

        assert_eq!(
            args,
            expected(
                "sync",
                (REMOTE, LOCAL),
                &[
                    "--max-delete",
                    "100",
                    "--create-empty-src-dirs",
                    "--drive-skip-gdocs",
                    "--backup-dir",
                    LOCAL_BACKUP,
                ],
                (RUN_CHANGES, RUN_LOG),
                &[],
            )
        );
    }

    #[test]
    fn only_a_normal_round_of_a_mirror_carries_the_cap() {
        let mirror = behavior(SyncMode::MirrorUp);
        let capped = |mode| args(&mirror, mode).contains(&"--max-delete".to_owned());

        assert!(capped(RoundMode::Normal));
        assert!(!capped(RoundMode::Resync));
        assert!(!capped(RoundMode::Force));
        assert!(!capped(RoundMode::DryRun(ReviewKind::Resync)));
        assert!(
            !args(&behavior(SyncMode::Download), RoundMode::Normal)
                .contains(&"--max-delete".to_owned())
        );
    }

    #[test]
    fn the_cap_is_a_share_of_the_files_the_last_round_left() {
        let mirror = Behavior {
            max_delete: 25,
            ..behavior(SyncMode::MirrorUp)
        };

        let args = args_with(&mirror, RoundMode::Normal, 41);

        let at = args.iter().position(|arg| arg == "--max-delete").unwrap();
        assert_eq!(args[at + 1], "10");
        assert_eq!(max_delete_limit(0, 50), 0);
        assert_eq!(max_delete_limit(u64::MAX, 100), u64::MAX / 100);
    }

    #[test]
    fn a_resync_and_a_force_round_run_like_a_normal_one_without_the_cap() {
        for mode in [RoundMode::Resync, RoundMode::Force] {
            assert_eq!(
                args(&behavior(SyncMode::MirrorDown), mode),
                expected(
                    "sync",
                    (REMOTE, LOCAL),
                    &[
                        "--create-empty-src-dirs",
                        "--drive-skip-gdocs",
                        "--backup-dir",
                        LOCAL_BACKUP,
                    ],
                    (RUN_CHANGES, RUN_LOG),
                    &[],
                )
            );
        }
    }

    #[test]
    fn a_dry_run_writes_its_report_and_log_beside_the_state_and_changes_nothing() {
        let args = args(
            &behavior(SyncMode::MirrorUp),
            RoundMode::DryRun(ReviewKind::Resync),
        );

        assert_eq!(
            args,
            expected(
                "sync",
                (LOCAL, REMOTE),
                &[
                    "--create-empty-src-dirs",
                    "--drive-skip-gdocs",
                    "--backup-dir",
                    REMOTE_BACKUP,
                ],
                (PREVIEW_CHANGES, PREVIEW_LOG),
                &["--dry-run"],
            )
        );
    }

    #[test]
    fn no_versions_means_no_backup_dir_in_any_mode() {
        let off = |mode| Behavior {
            versions: false,
            ..behavior(mode)
        };

        for mode in [
            SyncMode::Upload,
            SyncMode::Download,
            SyncMode::MirrorUp,
            SyncMode::MirrorDown,
        ] {
            assert!(!args(&off(mode), RoundMode::Normal).contains(&"--backup-dir".to_owned()));
        }
    }

    #[test]
    fn empty_dirs_off_drops_the_flag_and_google_docs_on_drops_the_skip() {
        let behavior = Behavior {
            empty_dirs: false,
            google_docs: GoogleDocs::Export,
            ..behavior(SyncMode::Download)
        };

        let args = args(&behavior, RoundMode::Normal);

        assert!(!args.contains(&"--create-empty-src-dirs".to_owned()));
        assert!(!args.contains(&"--drive-skip-gdocs".to_owned()));
    }

    #[test]
    fn the_bandwidth_limit_is_the_one_bisync_uses() {
        let limited = Behavior {
            limit_upload: Bandwidth::KiBPerSec(500),
            limit_download: Bandwidth::Off,
            ..behavior(SyncMode::Upload)
        };

        let limited_args = args(&limited, RoundMode::Normal);

        let at = limited_args
            .iter()
            .position(|arg| arg == "--bwlimit")
            .unwrap();
        let wanted: Vec<String> = bwlimit_args(&limited)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(limited_args[at..at + 2], wanted[..]);
        assert!(
            !args(&behavior(SyncMode::Upload), RoundMode::Normal).contains(&"--bwlimit".to_owned())
        );
    }

    #[test]
    fn the_remote_versions_folder_is_named_by_the_account_and_the_sync() {
        assert_eq!(
            remote_versions_dir("gdrive:Notes/Work", "drive").as_deref(),
            Some("gdrive:.syncfast-versions/drive")
        );
        assert_eq!(remote_versions_dir("no-account", "drive"), None);
        assert_eq!(remote_versions_dir(":x", "drive"), None);
    }

    fn classify_fixture(name: &str) -> Outcome {
        classify_oneway(exit_code(name), &fixture(name))
    }

    #[test]
    fn every_clean_fixture_is_ok() {
        for name in [
            "oneway_copy_new",
            "oneway_copy_changed",
            "oneway_sync_delete",
            "oneway_sync_delete_dry_run",
            "oneway_sync_backup_dir",
            "oneway_sync_empty_source",
        ] {
            assert_eq!(classify_fixture(name), Outcome::Ok, "{name}");
        }
    }

    #[test]
    fn the_max_delete_abort_is_too_many_deletes() {
        assert_eq!(
            classify_fixture("oneway_sync_max_delete"),
            Outcome::TooManyDeletes
        );
    }

    #[test]
    fn a_source_file_that_cannot_be_read_is_transient() {
        assert_eq!(classify_fixture("oneway_sync_errors"), Outcome::Transient);
    }

    fn failing(level: &str, msg: &str) -> Vec<LogLine> {
        crate::bisync_log::parse_log(&format!(
            "{{\"time\":\"2026-11-07T16:36:13.2-03:00\",\"level\":\"{level}\",\"msg\":\"{msg}\"}}\n"
        ))
    }

    #[test]
    fn a_lost_sign_in_is_an_auth_error_even_next_to_other_failures() {
        let lines = failing("error", "oauth2: cannot fetch token: invalid_grant");

        assert_eq!(classify_oneway(1, &lines), Outcome::AuthError);
    }

    #[test]
    fn a_fault_the_user_can_fix_needs_attention_with_the_message_that_named_it() {
        let lines = failing("error", "googleapi: storageQuotaExceeded");

        assert_eq!(
            classify_oneway(1, &lines),
            Outcome::NeedsAttention {
                fault: Fault::Quota,
                detail: "googleapi: storageQuotaExceeded".to_owned(),
            }
        );
    }

    #[test]
    fn rclone_rejecting_the_arguments_is_a_config_error() {
        let lines = failing("error", "unknown flag: --nope");

        assert_eq!(classify_oneway(2, &lines), Outcome::ConfigError);
        assert_eq!(classify_oneway(1, &lines), Outcome::Transient);
    }

    #[test]
    fn a_failure_to_delete_is_too_many_deletes_only_after_a_fatal_exit() {
        let threshold = failing(
            "error",
            "Got fatal error on delete: --max-delete threshold reached",
        );
        let fatal = failing("error", "Got fatal error on delete: other");

        assert_eq!(classify_oneway(7, &threshold), Outcome::TooManyDeletes);
        assert_eq!(classify_oneway(7, &fatal), Outcome::TooManyDeletes);
        assert_eq!(classify_oneway(1, &threshold), Outcome::Transient);
    }

    fn changes(name: &str) -> String {
        std::fs::read_to_string(fixture_path(&format!("{name}.changes"))).unwrap()
    }

    fn summary(name: &str, mode: SyncMode) -> RunSummary {
        summarize_oneway(&changes(name), mode, exit_code(name))
    }

    fn change(kind: ChangeKind, path: &str) -> Change {
        Change {
            kind,
            path: path.to_owned(),
        }
    }

    #[test]
    fn a_copy_reports_what_it_writes_and_ignores_what_it_would_leave_behind() {
        let up = summary("oneway_copy_new", SyncMode::Upload);
        let down = summary("oneway_copy_new", SyncMode::Download);

        assert_eq!(
            up.changes,
            [
                change(ChangeKind::OverwriteRemote, "changed.txt"),
                change(ChangeKind::Upload, "new.txt"),
                change(ChangeKind::Upload, "dir/nested.txt"),
            ]
        );
        assert_eq!(
            (
                up.uploaded,
                up.downloaded,
                up.deleted_local,
                up.deleted_remote
            ),
            (Some(3), Some(0), Some(0), Some(0))
        );
        assert_eq!(
            down.changes,
            [
                change(ChangeKind::OverwriteLocal, "changed.txt"),
                change(ChangeKind::Download, "new.txt"),
                change(ChangeKind::Download, "dir/nested.txt"),
            ]
        );
        assert_eq!((down.uploaded, down.downloaded), (Some(0), Some(3)));
        assert!(changes("oneway_copy_new").contains("- extra.txt"));
    }

    #[test]
    fn a_mirror_counts_the_files_it_deletes_on_the_side_it_writes() {
        let up = summary("oneway_sync_delete", SyncMode::MirrorUp);
        let down = summary("oneway_sync_delete", SyncMode::MirrorDown);

        assert!(
            up.changes
                .contains(&change(ChangeKind::DeleteRemote, "extra.txt"))
        );
        assert_eq!((up.deleted_remote, up.deleted_local), (Some(1), Some(0)));
        assert_eq!((up.uploaded, up.downloaded), (Some(3), Some(0)));
        assert!(
            down.changes
                .contains(&change(ChangeKind::DeleteLocal, "extra.txt"))
        );
        assert_eq!(
            (down.deleted_local, down.deleted_remote),
            (Some(1), Some(0))
        );
        assert_eq!((down.uploaded, down.downloaded), (Some(0), Some(3)));
    }

    #[test]
    fn a_dry_run_report_summarizes_like_the_real_one() {
        let sorted = |mut summary: RunSummary| {
            summary.changes.sort_by(|a, b| a.path.cmp(&b.path));
            summary
        };

        let real = summary("oneway_sync_delete", SyncMode::MirrorUp);
        let dry = summary("oneway_sync_delete_dry_run", SyncMode::MirrorUp);

        assert_eq!(sorted(dry), sorted(real));
    }

    #[test]
    fn a_run_with_a_backup_dir_reports_the_replaced_file_as_an_overwrite() {
        let summary = summary("oneway_sync_backup_dir", SyncMode::MirrorUp);

        assert!(
            summary
                .changes
                .contains(&change(ChangeKind::OverwriteRemote, "changed.txt"))
        );
    }

    #[test]
    fn a_run_that_stopped_has_no_counters_and_leaves_out_the_files_it_did_not_delete() {
        let stopped = summary("oneway_sync_max_delete", SyncMode::MirrorUp);

        assert_eq!(
            stopped.changes,
            [
                change(ChangeKind::DeleteRemote, "f2.txt"),
                change(ChangeKind::Upload, "keep.txt"),
            ]
        );
        assert_eq!((stopped.uploaded, stopped.deleted_remote), (None, None));
    }

    #[test]
    fn a_file_that_could_not_be_read_is_not_a_change() {
        let errors = summary("oneway_sync_errors", SyncMode::Upload);

        assert_eq!(errors.changes, [change(ChangeKind::Upload, "ok.txt")]);
        assert_eq!(errors.uploaded, None);
    }

    #[test]
    fn an_empty_source_mirror_deletes_everything_and_says_so() {
        let summary = summary("oneway_sync_empty_source", SyncMode::MirrorUp);

        assert_eq!(summary.deleted_remote, Some(3));
        assert_eq!(summary.changes.len(), 3);
    }

    #[test]
    fn the_summary_reads_symbol_space_path_and_keeps_spaces_in_paths() {
        let text = "+ a b.txt\n* dir/c.txt\n= same.txt\nnot a line\n+nospace\n";

        let summary = summarize_oneway(text, SyncMode::Download, 0);

        assert_eq!(
            summary.changes,
            [
                change(ChangeKind::Download, "a b.txt"),
                change(ChangeKind::OverwriteLocal, "dir/c.txt"),
            ]
        );
        assert_eq!(summary.conflicts_new, None);
    }

    #[test]
    fn the_destination_holds_what_was_there_what_arrived_and_what_was_replaced() {
        let text = "= a\n+ b\n* c\n- d\n! e\n+ e\n";

        assert_eq!(files_at_destination(text), 3);
        assert_eq!(files_at_destination(""), 0);
    }

    #[test]
    fn the_destination_count_of_a_mirror_fixture_matches_what_stays() {
        assert_eq!(files_at_destination(&changes("oneway_sync_delete")), 4);
        assert_eq!(
            files_at_destination(&changes("oneway_sync_empty_source")),
            0
        );
    }
}
