//! The contract with `rclone bisync`: the fixed argument list and the
//! classification of how a run ended. See ADR 0002.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use jiff::Zoned;
use serde::{Deserialize, Serialize};

use crate::behavior::{Behavior, GoogleDocs};
use crate::bisync_log::LogLine;
use crate::config::Pair;
use crate::paths::Paths;

const MAX_LOCK: &str = "2m";
const STATS_INTERVAL: &str = "2s";
const STAMP_FORMAT: &str = "%Y%m%d-%H%M%S";
const AUTH_PATTERNS: [&str; 4] = [
    "invalid_grant",
    "couldn't fetch token",
    "oauth2: cannot fetch token",
    "token expired",
];
const TOO_MANY_DELETES: &str = "too many deletes";
const ALL_FILES_CHANGED: &str = "Safety abort: all files were changed";
const MUST_RESYNC: &str = "Must run --resync to recover";
const EMPTY_PRIOR_LISTINGS: [&str; 2] = ["Empty prior Path1 listing", "Empty prior Path2 listing"];
const FAILED_TO_BISYNC: &str = "Failed to bisync";
const EXIT_NOT_CRITICAL: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_CRITICAL: i32 = 7;

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
pub enum Mode {
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
pub fn log_file(paths: &Paths, pair_id: &str, mode: Mode, now: &Zoned) -> PathBuf {
    let stamp = stamp(now);
    match mode {
        Mode::DryRun(_) => paths
            .pair_state_dir(pair_id)
            .join(format!("preview-{stamp}.jsonl")),
        Mode::Normal | Mode::Resync | Mode::Force => {
            paths.runs_dir(pair_id).join(format!("{stamp}.jsonl"))
        }
    }
}

fn dir_arg(path: &Path) -> OsString {
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
    mode: Mode,
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
        "--max-delete".into(),
        behavior.max_delete.to_string().into(),
    ];
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
    args.extend([
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

fn mode_args(behavior: &Behavior, mode: Mode) -> Vec<OsString> {
    let resync = || -> Vec<OsString> {
        vec![
            "--resync".into(),
            "--resync-mode".into(),
            behavior.resync_mode.rclone_value().into(),
        ]
    };
    match mode {
        Mode::Normal => Vec::new(),
        Mode::Resync => resync(),
        Mode::Force => vec!["--force".into()],
        Mode::DryRun(kind) => {
            let mut args = match kind {
                ReviewKind::Resync => resync(),
                ReviewKind::Force => vec!["--force".into()],
            };
            args.push("--dry-run".into());
            args
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeedsReviewCause {
    CriticalError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    NeedsReview(NeedsReviewCause),
    TooManyDeletes,
    /// rclone's safety abort when every file of a side changed at once;
    /// like [`Outcome::TooManyDeletes`], `--force` lets it through.
    AllFilesChanged,
    AuthError,
    /// rclone rejected the arguments: a syncfast bug.
    ConfigError,
    Transient,
}

fn has_lst_err(workdir: &Path) -> bool {
    std::fs::read_dir(workdir).is_ok_and(|entries| {
        entries
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".lst-err"))
    })
}

/// Lines that can say why a run failed: errors, anything that was not JSON,
/// and the closing `Failed to bisync` notice. Info and dry-run notices also
/// quote file names, which must not be mistaken for error text.
fn failure_msgs(lines: &[LogLine]) -> impl Iterator<Item = &str> {
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
    if has_lst_err(workdir) {
        return Outcome::NeedsReview(NeedsReviewCause::CriticalError);
    }
    if failure_msgs(lines).any(|msg| {
        let msg = msg.to_lowercase();
        AUTH_PATTERNS.iter().any(|pattern| msg.contains(pattern))
    }) {
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
    use crate::behavior::{ConflictLoser, ConflictResolve, ResyncMode};
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
        }
    }

    fn args(behavior: &Behavior, mode: Mode) -> Vec<String> {
        bisync_args(&pair(), behavior, &paths(), mode, &now())
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    const COMMON: [&str; 28] = [
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
        "--stats",
        "2s",
        "--stats-log-level",
        "NOTICE",
        "--use-json-log",
        "--log-file",
        "/s/syncfast/pairs/drive/runs/20260102-150405.jsonl",
    ];

    fn expected(log: &str, mode_flags: &[&str]) -> Vec<String> {
        let mut args: Vec<String> = COMMON.iter().map(|arg| (*arg).to_owned()).collect();
        *args.last_mut().unwrap() = log.to_owned();
        args.push("-v".to_owned());
        args.extend(mode_flags.iter().map(|arg| (*arg).to_owned()));
        args
    }

    const RUN_LOG: &str = "/s/syncfast/pairs/drive/runs/20260102-150405.jsonl";
    const PREVIEW_LOG: &str = "/s/syncfast/pairs/drive/preview-20260102-150405.jsonl";

    #[test]
    fn normal_mode_uses_only_the_fixed_arguments() {
        let args = args(&Behavior::default(), Mode::Normal);

        assert_eq!(args, expected(RUN_LOG, &[]));
    }

    #[test]
    fn resync_mode_adds_resync_with_the_newer_policy() {
        let args = args(&Behavior::default(), Mode::Resync);

        assert_eq!(
            args,
            expected(RUN_LOG, &["--resync", "--resync-mode", "newer"])
        );
    }

    #[test]
    fn force_mode_adds_force() {
        let args = args(&Behavior::default(), Mode::Force);

        assert_eq!(args, expected(RUN_LOG, &["--force"]));
    }

    #[test]
    fn resync_dry_run_adds_dry_run_and_logs_to_the_preview_file() {
        let args = args(&Behavior::default(), Mode::DryRun(ReviewKind::Resync));

        assert_eq!(
            args,
            expected(
                PREVIEW_LOG,
                &["--resync", "--resync-mode", "newer", "--dry-run"]
            )
        );
    }

    #[test]
    fn force_dry_run_adds_force_and_dry_run_and_logs_to_the_preview_file() {
        let args = args(&Behavior::default(), Mode::DryRun(ReviewKind::Force));

        assert_eq!(args, expected(PREVIEW_LOG, &["--force", "--dry-run"]));
    }

    #[test]
    fn log_file_is_the_one_in_the_arguments() {
        let normal = log_file(&paths(), "drive", Mode::Normal, &now());
        let preview = log_file(&paths(), "drive", Mode::DryRun(ReviewKind::Force), &now());

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

        let args = args(&behavior, Mode::Resync);

        let value_of = |flag: &str| {
            let at = args.iter().position(|arg| arg == flag).unwrap();
            args[at + 1].clone()
        };
        assert_eq!(value_of("--conflict-resolve"), "path1");
        assert_eq!(value_of("--conflict-loser"), "pathname");
        assert_eq!(value_of("--max-delete"), "20");
        assert_eq!(value_of("--resync-mode"), "path2");
    }

    #[test]
    fn switched_off_behaviors_drop_their_flags() {
        let behavior = Behavior {
            versions: false,
            empty_dirs: false,
            google_docs: GoogleDocs::Export,
            ..Behavior::default()
        };

        let args = args(&behavior, Mode::Normal);

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
