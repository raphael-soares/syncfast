//! What a restore asks of restic and how it ends (spec phase 3, step 9):
//! the three argument forms, the folder a restore goes into and the lines
//! restic prints while it runs. No I/O except the check for a free name.

use std::path::{Path, PathBuf};

use jiff::Zoned;
use serde::{Deserialize, Serialize};

use crate::restic::{EXIT_INTERRUPTED, ResticError, classify_failure};

pub const NOTHING_MATCHES: &str = "Nothing in the snapshot matches that path.";
pub const DESTINATION_EXISTS: &str = "Restore destination already exists.";
pub const BACKUP_BUSY: &str = "The backup is busy. Try again in a few minutes.";
pub const CANCELLED: &str =
    "The restore was cancelled. What was already restored stays in the folder.";

const LONGEST_SNAPSHOT_REF: usize = 64;
const SHORTEST_SNAPSHOT_REF: usize = 4;
const NAME_TRIES: u32 = 1000;

/// Which part of the snapshot a restore takes, and where it is read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum RestoreForm {
    /// Everything. `root` is the one folder the snapshot was made of: its
    /// content goes straight into the destination. Without it (several
    /// roots) the absolute paths go under the destination.
    Whole { root: Option<String> },
    /// The chosen items, each as `/<path relative to parent>`, read from
    /// `parent`.
    Items {
        parent: String,
        includes: Vec<String>,
    },
}

/// `snapshot` is `latest` or an id of 4 to 64 hex digits.
///
/// # Errors
/// What the text is, when it is neither.
pub fn check_snapshot_ref(snapshot: &str) -> Result<(), String> {
    let hex = (SHORTEST_SNAPSHOT_REF..=LONGEST_SNAPSHOT_REF).contains(&snapshot.len())
        && snapshot.bytes().all(|byte| byte.is_ascii_hexdigit());
    if snapshot == "latest" || hex {
        Ok(())
    } else {
        Err(format!(
            "\"{snapshot}\" is not a snapshot id or \"latest\"."
        ))
    }
}

/// The parts of an absolute path of the snapshot.
fn components(path: &str) -> Result<Vec<&str>, String> {
    let bad = || format!("\"{path}\" is not an absolute path of the snapshot.");
    if !path.starts_with('/') || path.contains('\0') {
        return Err(bad());
    }
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.iter().any(|part| matches!(*part, "." | "..")) {
        return Err(bad());
    }
    Ok(parts)
}

/// restic's include patterns treat these four characters as syntax.
fn escape_pattern(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// The form for what was asked: no `requested` paths is the whole
/// snapshot; paths are read from their deepest common folder.
///
/// # Errors
/// A path is not an absolute path of the snapshot, or it is the root `/`.
pub fn restore_form(
    snapshot_paths: &[String],
    requested: &[String],
) -> Result<RestoreForm, String> {
    if requested.is_empty() {
        return Ok(RestoreForm::Whole {
            root: match snapshot_paths {
                [root] => Some(root.clone()),
                _ => None,
            },
        });
    }
    let mut items = Vec::with_capacity(requested.len());
    for path in requested {
        let parts = components(path)?;
        if parts.is_empty() {
            return Err("Choose files or folders inside the snapshot.".to_owned());
        }
        items.push(parts);
    }
    let folders: Vec<&[&str]> = items.iter().map(|item| &item[..item.len() - 1]).collect();
    let common = folders.iter().fold(folders[0].len(), |common, folder| {
        let shared = folder
            .iter()
            .zip(folders[0])
            .take_while(|(a, b)| a == b)
            .count();
        common.min(shared)
    });
    let parent = format!("/{}", items[0][..common].join("/"));
    let includes = items
        .iter()
        .map(|item| escape_pattern(&format!("/{}", item[common..].join("/"))))
        .collect();
    Ok(RestoreForm::Items { parent, includes })
}

/// The arguments of `restic restore` for `snapshot` into `target`.
#[must_use]
pub fn restore_args(
    snapshot: &str,
    form: &RestoreForm,
    target: &str,
    dry_run: bool,
) -> Vec<String> {
    let from = match form {
        RestoreForm::Whole { root: Some(root) } => format!("{snapshot}:{root}"),
        RestoreForm::Items { parent, .. } => format!("{snapshot}:{parent}"),
        RestoreForm::Whole { root: None } => snapshot.to_owned(),
    };
    let mut args = vec![
        "restore".to_owned(),
        from,
        "--target".to_owned(),
        target.to_owned(),
    ];
    args.push(if dry_run { "--dry-run" } else { "--verify" }.to_owned());
    args.push("--json".to_owned());
    if let RestoreForm::Items { includes, .. } = form {
        for include in includes {
            args.push("--include".to_owned());
            args.push(include.clone());
        }
    }
    args
}

fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| {
            if matches!(ch, '/' | '\0') || ch.is_control() {
                '-'
            } else {
                ch
            }
        })
        .collect();
    cleaned.trim().to_owned()
}

/// `Documents 2026-11-08 1430`.
#[must_use]
fn restore_folder_name(backup_name: &str, now: &Zoned) -> String {
    format!(
        "{} {}",
        clean_name(backup_name),
        now.strftime("%Y-%m-%d %H%M")
    )
}

/// The folder a restore goes into, inside `parent`: the dated name, with
/// ` 2`, ` 3` after it while that name is taken.
///
/// # Errors
/// `parent` is not a folder, or every name is taken.
pub async fn new_restore_folder(
    parent: &Path,
    backup_name: &str,
    now: &Zoned,
) -> Result<PathBuf, String> {
    match tokio::fs::metadata(parent).await {
        Ok(meta) if meta.is_dir() => {}
        _ => return Err(format!("{} is not a folder.", parent.display())),
    }
    let base = restore_folder_name(backup_name, now);
    for number in 1..=NAME_TRIES {
        let name = if number == 1 {
            base.clone()
        } else {
            format!("{base} {number}")
        };
        let candidate = parent.join(name);
        if tokio::fs::symlink_metadata(&candidate).await.is_err() {
            return Ok(candidate);
        }
    }
    Err(format!(
        "Every name for the new folder in {} is taken.",
        parent.display()
    ))
}

/// restic's `status` line while a restore runs, and the progress file the
/// windows read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RestoreProgress {
    #[serde(default)]
    pub percent_done: Option<f64>,
    #[serde(default)]
    pub files_restored: u64,
    #[serde(default)]
    pub total_files: u64,
    #[serde(default)]
    pub bytes_restored: u64,
    #[serde(default)]
    pub total_bytes: u64,
}

/// The `summary` line: how a restore that ran to the end went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct RestoreSummary {
    #[serde(default)]
    pub total_files: u64,
    #[serde(default)]
    pub files_restored: u64,
    #[serde(default)]
    pub total_bytes: u64,
    #[serde(default)]
    pub bytes_restored: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RestoreLine {
    Status(RestoreProgress),
    Summary(RestoreSummary),
    Other,
}

#[derive(Deserialize)]
struct Tagged {
    message_type: String,
}

/// What a line of `restic restore --json` is.
#[must_use]
pub fn parse_restore_line(line: &str) -> RestoreLine {
    let Ok(tagged) = serde_json::from_str::<Tagged>(line) else {
        return RestoreLine::Other;
    };
    match tagged.message_type.as_str() {
        "status" => serde_json::from_str(line).map_or(RestoreLine::Other, RestoreLine::Status),
        "summary" => serde_json::from_str(line).map_or(RestoreLine::Other, RestoreLine::Summary),
        _ => RestoreLine::Other,
    }
}

/// How a finished restic restore counts: the numbers, or what to tell the
/// user. `files_restored` never exceeds `total_files`: restic counts the
/// folders it makes on the way to a chosen item on one side only.
///
/// # Errors
/// The text for the user when the restore did not work.
pub fn classify_restore(
    exit_code: Option<i32>,
    summary: Option<&RestoreSummary>,
    stderr: &[u8],
) -> Result<RestoreSummary, String> {
    match (exit_code, summary) {
        (Some(0), Some(summary)) if summary.total_files == 0 => Err(NOTHING_MATCHES.to_owned()),
        (Some(0), Some(summary)) => Ok(RestoreSummary {
            files_restored: summary.files_restored.min(summary.total_files),
            bytes_restored: summary.bytes_restored.min(summary.total_bytes),
            ..*summary
        }),
        (Some(0), None) => Err("restic finished without saying what it restored".to_owned()),
        (Some(EXIT_INTERRUPTED), _) => Err(CANCELLED.to_owned()),
        (code, _) => Err(match classify_failure(code, stderr) {
            ResticError::Locked { .. } => BACKUP_BUSY.to_owned(),
            ResticError::Failed { message, .. } => message,
            other => other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn items(parent: &str, includes: &[&str]) -> RestoreForm {
        RestoreForm::Items {
            parent: parent.to_owned(),
            includes: strings(includes),
        }
    }

    #[test]
    fn a_whole_snapshot_of_one_root_is_read_from_that_root() {
        let form = restore_form(&strings(&["/home/ana/Documents"]), &[]).unwrap();

        assert_eq!(
            restore_args("abcd1234", &form, "/tmp/out", false),
            [
                "restore",
                "abcd1234:/home/ana/Documents",
                "--target",
                "/tmp/out",
                "--verify",
                "--json"
            ]
        );
    }

    #[test]
    fn a_whole_snapshot_of_several_roots_keeps_their_absolute_paths() {
        let form = restore_form(&strings(&["/home/ana/a", "/srv/b"]), &[]).unwrap();

        assert_eq!(
            restore_args("latest", &form, "/tmp/out", false),
            [
                "restore", "latest", "--target", "/tmp/out", "--verify", "--json"
            ]
        );
    }

    #[test]
    fn chosen_items_are_read_from_their_common_folder_one_include_each() {
        let form = restore_form(
            &strings(&["/home/ana"]),
            &strings(&["/home/ana/docs/a.txt", "/home/ana/pics"]),
        )
        .unwrap();

        assert_eq!(form, items("/home/ana", &["/docs/a.txt", "/pics"]));
        assert_eq!(
            restore_args("abcd1234", &form, "/tmp/out", false),
            [
                "restore",
                "abcd1234:/home/ana",
                "--target",
                "/tmp/out",
                "--verify",
                "--json",
                "--include",
                "/docs/a.txt",
                "--include",
                "/pics"
            ]
        );
    }

    #[test]
    fn a_dry_run_does_not_verify() {
        let form = restore_form(&strings(&["/a", "/b"]), &[]).unwrap();

        let args = restore_args("latest", &form, "/tmp/out", true);

        assert!(args.contains(&"--dry-run".to_owned()));
        assert!(!args.contains(&"--verify".to_owned()));
    }

    #[test]
    fn a_path_with_a_space_and_brackets_is_escaped_for_restic() {
        let form = restore_form(
            &strings(&["/home/ana"]),
            &strings(&["/home/ana/My Docs/a [1].txt", "/home/ana/x*y?z\\w"]),
        )
        .unwrap();

        assert_eq!(
            form,
            items("/home/ana", &["/My Docs/a \\[1].txt", "/x\\*y\\?z\\\\w"])
        );
    }

    #[test]
    fn the_parent_of_items_in_different_folders_is_their_deepest_shared_folder() {
        let form = restore_form(
            &strings(&["/data"]),
            &strings(&["/data/a/x/1.txt", "/data/a/y/2.txt", "/data/a/z"]),
        )
        .unwrap();

        assert_eq!(form, items("/data/a", &["/x/1.txt", "/y/2.txt", "/z"]));
    }

    #[test]
    fn items_in_unrelated_folders_are_read_from_the_root_of_the_snapshot() {
        let form = restore_form(&strings(&["/a", "/b"]), &strings(&["/a/1", "/b/2"])).unwrap();

        assert_eq!(form, items("/", &["/a/1", "/b/2"]));
        assert_eq!(restore_args("latest", &form, "/t", false)[1], "latest:/");
    }

    #[test]
    fn one_item_is_read_from_the_folder_that_holds_it() {
        let form = restore_form(&[], &strings(&["/home/ana/docs"])).unwrap();

        assert_eq!(form, items("/home/ana", &["/docs"]));
    }

    #[test]
    fn an_item_inside_another_is_asked_for_too() {
        let form = restore_form(&[], &strings(&["/d/a", "/d/a/b"])).unwrap();

        assert_eq!(form, items("/d", &["/a", "/a/b"]));
    }

    #[test]
    fn a_trailing_slash_or_a_doubled_one_is_the_same_path() {
        let form = restore_form(&[], &strings(&["/d//a/"])).unwrap();

        assert_eq!(form, items("/d", &["/a"]));
    }

    #[test]
    fn paths_that_are_not_absolute_paths_of_the_snapshot_are_refused() {
        for bad in ["docs/a", "", "/d/../etc", "/d/./a", "/d/a\0b"] {
            assert!(
                restore_form(&[], &strings(&[bad])).is_err(),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn the_root_of_the_snapshot_is_not_an_item() {
        assert!(restore_form(&[], &strings(&["/"])).is_err());
    }

    #[test]
    fn only_latest_and_hex_ids_name_a_snapshot() {
        for good in ["latest", "abcd", "ABCD1234", &"a".repeat(64)] {
            assert!(check_snapshot_ref(good).is_ok(), "{good}");
        }
        for bad in ["abc", &"a".repeat(65), "--help", "abcg", "", "latest2"] {
            assert!(check_snapshot_ref(bad).is_err(), "{bad}");
        }
    }

    fn moment() -> Zoned {
        "2026-11-08T14:30:59-03:00[America/Sao_Paulo]"
            .parse()
            .unwrap()
    }

    #[test]
    fn the_new_folder_is_named_for_the_backup_and_the_minute() {
        assert_eq!(
            restore_folder_name("Documents", &moment()),
            "Documents 2026-11-08 1430"
        );
    }

    #[test]
    fn a_slash_in_the_backup_name_does_not_make_a_second_folder() {
        assert_eq!(
            restore_folder_name(" a/b\nc ", &moment()),
            "a-b-c 2026-11-08 1430"
        );
    }

    #[tokio::test]
    async fn a_taken_name_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        let first = new_restore_folder(dir.path(), "Docs", &moment())
            .await
            .unwrap();
        assert_eq!(first, dir.path().join("Docs 2026-11-08 1430"));

        tokio::fs::create_dir(&first).await.unwrap();
        let second = new_restore_folder(dir.path(), "Docs", &moment())
            .await
            .unwrap();
        assert_eq!(second, dir.path().join("Docs 2026-11-08 1430 2"));

        tokio::fs::create_dir(&second).await.unwrap();
        let third = new_restore_folder(dir.path(), "Docs", &moment())
            .await
            .unwrap();
        assert_eq!(third, dir.path().join("Docs 2026-11-08 1430 3"));
    }

    #[tokio::test]
    async fn a_parent_that_is_not_a_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        tokio::fs::write(&file, "x").await.unwrap();

        assert!(new_restore_folder(&file, "Docs", &moment()).await.is_err());
        assert!(
            new_restore_folder(&dir.path().join("none"), "Docs", &moment())
                .await
                .is_err()
        );
    }

    #[test]
    fn a_status_line_is_progress() {
        let line = r#"{"message_type":"status","seconds_elapsed":2,"percent_done":0.5,"total_files":6,"files_restored":3,"total_bytes":100,"bytes_restored":50}"#;

        assert_eq!(
            parse_restore_line(line),
            RestoreLine::Status(RestoreProgress {
                percent_done: Some(0.5),
                files_restored: 3,
                total_files: 6,
                bytes_restored: 50,
                total_bytes: 100,
            })
        );
    }

    #[test]
    fn a_summary_line_is_the_result_and_one_without_numbers_is_zero_files() {
        let line = r#"{"message_type":"summary","total_files":12,"files_restored":12,"total_bytes":41,"bytes_restored":41}"#;
        assert_eq!(
            parse_restore_line(line),
            RestoreLine::Summary(RestoreSummary {
                total_files: 12,
                files_restored: 12,
                total_bytes: 41,
                bytes_restored: 41,
            })
        );

        assert_eq!(
            parse_restore_line(r#"{"message_type":"summary"}"#),
            RestoreLine::Summary(RestoreSummary {
                total_files: 0,
                files_restored: 0,
                total_bytes: 0,
                bytes_restored: 0,
            })
        );
    }

    #[test]
    fn other_lines_are_not_progress() {
        for line in [
            "",
            "not json",
            r#"{"message_type":"verbose_status"}"#,
            r#"{"message_type":"exit_error","code":1,"message":"x"}"#,
            r#"{"message_type":"status","total_files":"many"}"#,
        ] {
            assert_eq!(parse_restore_line(line), RestoreLine::Other, "{line}");
        }
    }

    fn summary(total_files: u64, files_restored: u64) -> RestoreSummary {
        RestoreSummary {
            total_files,
            files_restored,
            total_bytes: 10,
            bytes_restored: 10,
        }
    }

    #[test]
    fn a_restore_that_matched_nothing_is_an_error() {
        assert_eq!(
            classify_restore(Some(0), Some(&summary(0, 0)), b""),
            Err(NOTHING_MATCHES.to_owned())
        );
    }

    #[test]
    fn a_restore_that_worked_reports_no_more_files_than_there_are() {
        assert_eq!(
            classify_restore(Some(0), Some(&summary(1, 10)), b""),
            Ok(summary(1, 1))
        );
        assert_eq!(
            classify_restore(Some(0), Some(&summary(12, 12)), b""),
            Ok(summary(12, 12))
        );
    }

    #[test]
    fn bytes_are_held_to_the_total_too() {
        let wide = RestoreSummary {
            bytes_restored: 99,
            ..summary(2, 2)
        };

        assert_eq!(
            classify_restore(Some(0), Some(&wide), b"")
                .unwrap()
                .bytes_restored,
            10
        );
    }

    #[test]
    fn exit_zero_without_a_summary_is_an_error() {
        assert!(classify_restore(Some(0), None, b"").is_err());
    }

    #[test]
    fn an_interrupted_restore_says_it_was_cancelled() {
        assert_eq!(
            classify_restore(Some(130), None, b""),
            Err(
                "The restore was cancelled. What was already restored stays in the folder."
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_locked_repository_is_a_busy_backup() {
        let stderr = br#"{"message_type":"exit_error","code":11,"message":"unable to create lock in backend: repository is already locked exclusively by PID 1"}"#;

        assert_eq!(
            classify_restore(Some(11), None, stderr),
            Err(BACKUP_BUSY.to_owned())
        );
    }

    #[test]
    fn other_failures_keep_restic_message() {
        let stderr = br#"{"message_type":"exit_error","code":1,"message":"verification failed"}"#;

        assert_eq!(
            classify_restore(Some(1), None, stderr),
            Err("verification failed".to_owned())
        );
        assert_eq!(
            classify_restore(Some(12), None, b""),
            Err("That key does not open this backup.".to_owned())
        );
    }
}
