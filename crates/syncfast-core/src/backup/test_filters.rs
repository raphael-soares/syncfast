//! `Test Filters` for a backup (spec phase 3, step 5): how many files and
//! bytes the rules leave out, counted by two dry runs into a throwaway
//! repository.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::config::BackupConfig;
use super::destination::Destination;
use super::key::{Key, KeyError};
use crate::restic::{EXIT_UNREADABLE, Repository, Restic, ResticError, classify_failure};

const TEST_KEY: &str = "SYNCFASTFILTERTESTSYNCFASTFILTERTESTSYNCFASTFILTERTE";
static NEXT_RUN: AtomicU64 = AtomicU64::new(0);

/// What the rules leave out of a backup. The totals are what the folders
/// hold without them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilterTest {
    pub excluded_files: u64,
    pub total_files: u64,
    pub excluded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum FilterTestError {
    #[error("cannot prepare the test repository: {0}")]
    Scratch(#[source] io::Error),
    #[error("a folder path is not valid UTF-8")]
    NonUtf8Path,
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Restic(#[from] ResticError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    files: u64,
    bytes: u64,
}

#[derive(Deserialize)]
struct Line {
    message_type: String,
    total_files_processed: Option<u64>,
    total_bytes_processed: Option<u64>,
}

/// The totals of the last `summary` line of a `backup --json` output.
fn parse_summary(stdout: &[u8]) -> Option<Counts> {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Line>(line).ok())
        .find(|line| line.message_type == "summary")
        .and_then(|line| {
            Some(Counts {
                files: line.total_files_processed?,
                bytes: line.total_bytes_processed?,
            })
        })
}

fn compare(without: Counts, with: Counts) -> FilterTest {
    FilterTest {
        excluded_files: without.files.saturating_sub(with.files),
        total_files: without.files,
        excluded_bytes: without.bytes.saturating_sub(with.bytes),
        total_bytes: without.bytes,
    }
}

fn dry_run_args<'a>(
    folders: &'a [&'a str],
    one_file_system: bool,
    exclude_file: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args = vec!["backup", "--dry-run", "--json", "--no-cache"];
    if one_file_system {
        args.push("--one-file-system");
    }
    if let Some(file) = exclude_file {
        args.extend(["--exclude-file", file]);
    }
    args.extend(folders);
    args
}

/// The folder the test works in; removed when it goes out of scope, which
/// includes a test cancelled by dropping its future.
struct Scratch(PathBuf);

impl Scratch {
    async fn create(parent: &Path) -> Result<Self, FilterTestError> {
        let run = NEXT_RUN.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!("filter-test-{}-{run}", std::process::id()));
        tokio::fs::create_dir_all(dir.join("work"))
            .await
            .map_err(FilterTestError::Scratch)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_dir_all(&self.0) {
            log::warn!("cannot remove {}: {err}", self.0.display());
        }
    }
}

async fn dry_run(
    restic: &Restic,
    repository: &Repository,
    key: &Key,
    args: &[&str],
) -> Result<Counts, FilterTestError> {
    let child = restic.spawn(repository, key, args).await?;
    let output = child.wait_with_output().await.map_err(ResticError::Spawn)?;
    match output.status.code() {
        Some(0 | EXIT_UNREADABLE) => {}
        code => return Err(classify_failure(code, &output.stderr).into()),
    }
    parse_summary(&output.stdout).ok_or_else(|| ResticError::BadOutput("backup summary").into())
}

/// Counts what `excludes` (the rendered `excludes.txt`) leaves out of the
/// folders of `backup`: a throwaway local repository under `scratch`, a
/// `backup --dry-run` without the file and one with it. Files restic cannot
/// read do not stop the count. Dropping the future stops restic and removes
/// the repository.
///
/// # Errors
/// The repository cannot be created, restic fails or prints no summary.
pub async fn run_filter_test(
    restic: &Restic,
    backup: &BackupConfig,
    excludes: &str,
    scratch: &Path,
) -> Result<FilterTest, FilterTestError> {
    let folders: Vec<&str> = backup
        .paths
        .iter()
        .map(|folder| folder.to_str().ok_or(FilterTestError::NonUtf8Path))
        .collect::<Result<_, _>>()?;
    let key = Key::parse(TEST_KEY)?;
    let dir = Scratch::create(scratch).await?;
    let exclude_path = dir.0.join("excludes.txt");
    tokio::fs::write(&exclude_path, excludes)
        .await
        .map_err(FilterTestError::Scratch)?;
    let exclude_file = exclude_path.to_str().ok_or(FilterTestError::NonUtf8Path)?;
    let repository = Repository::new(Destination::Local(dir.0.join("repo")), dir.0.join("work"));
    restic
        .run(&repository, &key, &["init", "--json", "--no-cache"], false)
        .await?;
    let without = dry_run(
        restic,
        &repository,
        &key,
        &dry_run_args(&folders, backup.one_file_system, None),
    )
    .await?;
    let with = dry_run(
        restic,
        &repository,
        &key,
        &dry_run_args(&folders, backup.one_file_system, Some(exclude_file)),
    )
    .await?;
    Ok(compare(without, with))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/restic-0.19.1")
            .join(name);
        std::fs::read(path).unwrap()
    }

    #[test]
    fn the_test_key_is_a_valid_backup_key() {
        assert!(Key::parse(TEST_KEY).is_ok());
    }

    #[test]
    fn the_summary_of_a_dry_run_gives_its_totals() {
        assert_eq!(
            parse_summary(&fixture("backup_dry_run_plain.out")),
            Some(Counts {
                files: 3,
                bytes: 18
            })
        );
        assert_eq!(
            parse_summary(&fixture("backup_dry_run_excluded.out")),
            Some(Counts { files: 1, bytes: 5 })
        );
    }

    #[test]
    fn the_last_summary_wins_and_other_lines_are_ignored() {
        let out = b"not json\n{\"message_type\":\"status\",\"total_files_processed\":9}\n{\"message_type\":\"summary\",\"total_files_processed\":1,\"total_bytes_processed\":2}\n{\"message_type\":\"summary\",\"total_files_processed\":4,\"total_bytes_processed\":8}\n{\"message_type\":\"verbose_status\"}\n";

        assert_eq!(parse_summary(out), Some(Counts { files: 4, bytes: 8 }));
    }

    #[test]
    fn output_without_a_complete_summary_gives_nothing() {
        assert_eq!(parse_summary(b""), None);
        assert_eq!(
            parse_summary(b"{\"message_type\":\"status\",\"total_files_processed\":1,\"total_bytes_processed\":1}\n"),
            None
        );
        assert_eq!(
            parse_summary(b"{\"message_type\":\"summary\",\"total_files_processed\":1}\n"),
            None
        );
    }

    #[test]
    fn the_result_is_the_difference_between_the_two_runs() {
        let without = parse_summary(&fixture("backup_dry_run_plain.out")).unwrap();
        let with = parse_summary(&fixture("backup_dry_run_excluded.out")).unwrap();

        assert_eq!(
            compare(without, with),
            FilterTest {
                excluded_files: 2,
                total_files: 3,
                excluded_bytes: 13,
                total_bytes: 18,
            }
        );
    }

    #[test]
    fn a_run_that_counts_more_with_the_rules_excludes_nothing() {
        assert_eq!(
            compare(Counts { files: 1, bytes: 1 }, Counts { files: 2, bytes: 5 }),
            FilterTest {
                excluded_files: 0,
                total_files: 1,
                excluded_bytes: 0,
                total_bytes: 1,
            }
        );
    }

    #[test]
    fn the_arguments_of_the_two_runs_differ_only_by_the_exclude_file() {
        let folders = ["/data/a", "/data/b"];

        assert_eq!(
            dry_run_args(&folders, true, None),
            [
                "backup",
                "--dry-run",
                "--json",
                "--no-cache",
                "--one-file-system",
                "/data/a",
                "/data/b",
            ]
        );
        assert_eq!(
            dry_run_args(&folders, false, Some("/tmp/x/excludes.txt")),
            [
                "backup",
                "--dry-run",
                "--json",
                "--no-cache",
                "--exclude-file",
                "/tmp/x/excludes.txt",
                "/data/a",
                "/data/b",
            ]
        );
    }

    #[tokio::test]
    async fn the_scratch_folder_exists_while_held_and_goes_away_when_dropped() {
        let root = tempfile::tempdir().unwrap();

        let first = Scratch::create(root.path()).await.unwrap();
        let second = Scratch::create(root.path()).await.unwrap();
        let (first_dir, second_dir) = (first.0.clone(), second.0.clone());

        assert_ne!(first_dir, second_dir);
        assert!(first_dir.join("work").is_dir());
        drop(first);
        assert!(!first_dir.exists());
        assert!(second_dir.exists());
    }
}
