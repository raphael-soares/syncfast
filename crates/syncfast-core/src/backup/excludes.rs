//! The `excludes.txt` restic reads for a backup (spec phase 3, step 5): the
//! free filters and presets of the sync, translated to restic's patterns.

use std::io;
use std::path::{Path, PathBuf};

use super::config::BackupConfig;
use crate::filters::{self, DirMatcher, GENERATED_HEADER, GIT_REPOS};

/// The repositories [`filters::find_git_repos`] found under one folder of a
/// backup, relative to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderRepos {
    pub folder: PathBuf,
    pub repos: Vec<String>,
}

fn preset_enabled(backup: &BackupConfig, name: &str) -> bool {
    backup.presets.iter().any(|preset| preset == name)
}

/// Backslash before each character restic's patterns read as syntax.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '*' | '?' | '[') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// A folder as the front of an anchored pattern: escaped, no trailing `/`.
fn folder_prefix(folder: &Path) -> String {
    let text = folder.to_string_lossy();
    escape(text.trim_end_matches('/'))
}

/// `--exclude-file` expands environment variables, so `$` is written `$$`.
fn push_line(out: &mut String, line: &str) {
    out.push_str(&line.replace('$', "$$"));
    out.push('\n');
}

fn push_rule(out: &mut String, rule: &str, folders: &[String]) {
    let Some(pattern) = rule.strip_prefix("- ") else {
        return;
    };
    let unslashed = pattern.trim_end_matches('/');
    if unslashed.is_empty() {
        return;
    }
    if pattern.starts_with('/') {
        for folder in folders {
            push_line(out, &format!("{folder}{unslashed}"));
        }
    } else {
        push_line(out, unslashed);
    }
}

/// The `excludes.txt` of `backup`: free filters first, then the enabled
/// presets in render order, then (with `git_repos`) one anchored line per
/// repository of `git_repos`. Preset names that do not exist are skipped;
/// config validation reports them.
#[must_use]
pub fn render_excludes(backup: &BackupConfig, git_repos: &[FolderRepos]) -> String {
    let folders: Vec<String> = backup
        .paths
        .iter()
        .map(|folder| folder_prefix(folder))
        .collect();
    let mut out = String::from(GENERATED_HEADER);
    out.push('\n');
    for rule in &backup.filters {
        push_rule(&mut out, rule, &folders);
    }
    for preset in &filters::PRESETS {
        if preset_enabled(backup, preset.name) {
            for rule in preset.rules {
                push_rule(&mut out, rule, &folders);
            }
        }
    }
    if preset_enabled(backup, GIT_REPOS) {
        for found in git_repos {
            let folder = folder_prefix(&found.folder);
            for repo in &found.repos {
                push_line(&mut out, &format!("{folder}/{}", escape(repo)));
            }
        }
    }
    out
}

/// The rules the repository scan skips: the rclone form [`DirMatcher`]
/// reads.
fn scan_rules(backup: &BackupConfig) -> String {
    let mut rules = String::new();
    for rule in &backup.filters {
        rules.push_str(rule);
        rules.push('\n');
    }
    for preset in &filters::PRESETS {
        if preset_enabled(backup, preset.name) {
            for rule in preset.rules {
                rules.push_str(rule);
                rules.push('\n');
            }
        }
    }
    rules
}

/// [`render_excludes`] with the repositories found now under each folder.
/// Without the `git_repos` preset it does no I/O. A failed scan renders no
/// repository lines.
pub async fn render_excludes_live(backup: &BackupConfig) -> String {
    if !preset_enabled(backup, GIT_REPOS) {
        return render_excludes(backup, &[]);
    }
    let skip = DirMatcher::new(&scan_rules(backup));
    let folders = backup.paths.clone();
    let found = tokio::task::spawn_blocking(move || {
        folders
            .into_iter()
            .map(|folder| FolderRepos {
                repos: filters::find_git_repos(&folder, &skip),
                folder,
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_else(|err| {
        log::warn!("git repository scan failed: {err}");
        Vec::new()
    });
    render_excludes(backup, &found)
}

/// Writes `contents` to `path` only when it differs from what is there, so
/// restic's view of the file and its modification time stay put between
/// rounds. Returns whether it wrote.
///
/// # Errors
/// The existing file cannot be read or the new one cannot be written.
pub fn write_excludes(path: &Path, contents: &str) -> io::Result<bool> {
    match std::fs::read(path) {
        Ok(current) if current == contents.as_bytes() => return Ok(false),
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    fastframe_file::write(path, contents)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::config::KeepSetting;

    fn backup(paths: &[&str], presets: &[&str], filters: &[&str]) -> BackupConfig {
        BackupConfig {
            id: "docs".into(),
            name: "Documents".into(),
            paths: paths.iter().map(PathBuf::from).collect(),
            dest: "/mnt/usb/docs".into(),
            every: "daily".into(),
            keep: KeepSetting::default(),
            presets: presets.iter().map(|name| (*name).to_owned()).collect(),
            filters: filters.iter().map(|line| (*line).to_owned()).collect(),
            one_file_system: true,
            limit_upload: "off".into(),
            limit_download: "off".into(),
            enabled: true,
        }
    }

    fn lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }

    fn only(filters: &[&str]) -> String {
        render_excludes(&backup(&["/data/docs"], &[], filters), &[])
    }

    #[test]
    fn the_file_starts_with_the_generated_header() {
        assert_eq!(
            lines(&only(&[]))[0],
            "# Generated by syncfast. Do not edit."
        );
        assert_eq!(lines(&only(&[])).len(), 1);
    }

    #[test]
    fn a_pattern_without_a_leading_slash_stays_as_it_is() {
        assert_eq!(lines(&only(&["- *.log"])), [GENERATED_HEADER, "*.log"]);
    }

    #[test]
    fn a_directory_pattern_loses_its_trailing_slash() {
        assert_eq!(
            lines(&only(&["- node_modules/"])),
            [GENERATED_HEADER, "node_modules"]
        );
    }

    #[test]
    fn a_pattern_with_a_slash_in_the_middle_stays_as_it_is() {
        assert_eq!(lines(&only(&["- a/b"])), [GENERATED_HEADER, "a/b"]);
        assert_eq!(lines(&only(&["- a/b/"])), [GENERATED_HEADER, "a/b"]);
    }

    #[test]
    fn an_anchored_pattern_becomes_one_line_per_folder() {
        let backup = backup(
            &["/home/ada/Documents", "/home/ada/Pictures/"],
            &[],
            &["- /cache/"],
        );

        assert_eq!(
            lines(&render_excludes(&backup, &[])),
            [
                GENERATED_HEADER,
                "/home/ada/Documents/cache",
                "/home/ada/Pictures/cache",
            ]
        );
    }

    #[test]
    fn an_anchored_pattern_keeps_the_rest_of_its_path() {
        let backup = backup(&["/data/docs"], &[], &["- /out/tmp"]);

        assert_eq!(
            lines(&render_excludes(&backup, &[]))[1],
            "/data/docs/out/tmp"
        );
    }

    #[test]
    fn special_characters_of_a_folder_are_escaped_in_its_anchored_lines() {
        let backup = backup(&["/home/ada/My [Docs]*?\\x"], &[], &["- /cache/"]);

        assert_eq!(
            lines(&render_excludes(&backup, &[]))[1],
            "/home/ada/My \\[Docs]\\*\\?\\\\x/cache"
        );
    }

    #[test]
    fn the_pattern_part_of_an_anchored_line_is_not_escaped() {
        let backup = backup(&["/data/docs"], &[], &["- /*.bak"]);

        assert_eq!(lines(&render_excludes(&backup, &[]))[1], "/data/docs/*.bak");
    }

    #[test]
    fn a_dollar_is_doubled_in_every_line() {
        let backup = backup(
            &["/data/$HOME docs"],
            &["system_junk"],
            &["- ~$*", "- /$tmp/"],
        );
        let text = render_excludes(&backup, &[]);

        assert!(lines(&text).contains(&"~$$*"), "{text}");
        assert!(lines(&text).contains(&"$$RECYCLE.BIN"), "{text}");
        assert!(lines(&text).contains(&"/data/$$HOME docs/$$tmp"), "{text}");
        assert!(
            !text
                .lines()
                .any(|line| line.contains('$') && !line.contains("$$"))
        );
    }

    #[test]
    fn lines_that_do_not_start_with_a_dash_and_a_space_are_left_out() {
        assert_eq!(
            lines(&only(&["+ keep", "*.log", "# note", "-no-space"])),
            [GENERATED_HEADER]
        );
    }

    #[test]
    fn free_filters_come_before_presets_and_presets_follow_the_render_order() {
        let backup = backup(
            &["/data/docs"],
            &["build_artifacts", "system_junk"],
            &["- *.bak"],
        );
        let text = render_excludes(&backup, &[]);
        let at = |line: &str| lines(&text).iter().position(|l| *l == line).unwrap();

        assert!(at("*.bak") < at(".directory"), "{text}");
        assert!(at(".directory") < at("venv"), "{text}");
        assert_eq!(at("*.bak"), 1);
    }

    #[test]
    fn a_preset_that_is_not_listed_adds_nothing() {
        let text = render_excludes(&backup(&["/data/docs"], &["build_artifacts"], &[]), &[]);

        assert!(!lines(&text).contains(&"*.log"));
        assert!(!lines(&text).contains(&".directory"));
        assert!(!lines(&text).contains(&".git"));
    }

    #[test]
    fn an_unknown_preset_is_skipped() {
        assert_eq!(
            lines(&render_excludes(&backup(&["/d"], &["nope"], &[]), &[])),
            [GENERATED_HEADER]
        );
    }

    #[test]
    fn build_artifacts_excludes_each_of_its_entries_and_never_git_or_build() {
        let text = render_excludes(&backup(&["/data/docs"], &["build_artifacts"], &[]), &[]);

        assert_eq!(
            lines(&text)[1..].to_vec(),
            [
                "node_modules",
                "target",
                ".venv",
                "venv",
                "__pycache__",
                ".gradle",
                ".cache",
                ".mypy_cache",
                ".pytest_cache",
                ".ruff_cache",
                ".tox",
                ".parcel-cache",
            ]
        );
        assert!(!lines(&text).contains(&".git"));
        assert!(!lines(&text).contains(&"build"));
    }

    #[test]
    fn dev_dirs_excludes_git_and_system_junk_keeps_it() {
        let dev = render_excludes(&backup(&["/d"], &["dev_dirs"], &[]), &[]);
        let junk = render_excludes(&backup(&["/d"], &["system_junk"], &[]), &[]);

        assert!(lines(&dev).contains(&".git"));
        assert!(!lines(&junk).contains(&".git"));
        assert!(lines(&junk).contains(&".Trash-*"));
    }

    #[test]
    fn git_repos_adds_one_anchored_line_per_repository_of_each_folder() {
        let backup = backup(&["/home/ada/code", "/srv/work"], &["git_repos"], &[]);
        let found = [
            FolderRepos {
                folder: "/home/ada/code".into(),
                repos: vec!["app".into(), "libs/core [old]".into()],
            },
            FolderRepos {
                folder: "/srv/work".into(),
                repos: vec!["site".into()],
            },
        ];

        assert_eq!(
            lines(&render_excludes(&backup, &found)),
            [
                GENERATED_HEADER,
                "/home/ada/code/app",
                "/home/ada/code/libs/core \\[old]",
                "/srv/work/site",
            ]
        );
    }

    #[test]
    fn repositories_are_ignored_without_the_git_repos_preset() {
        let found = [FolderRepos {
            folder: "/d".into(),
            repos: vec!["app".into()],
        }];

        assert_eq!(
            lines(&render_excludes(
                &backup(&["/d"], &["system_junk"], &[]),
                &found
            ))
            .iter()
            .filter(|line| line.contains("app"))
            .count(),
            0
        );
    }

    #[tokio::test]
    async fn the_live_render_finds_the_repositories_under_each_folder() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        for dir in [
            first.join("app/.git"),
            first.join("nested/lib/.git"),
            first.join("plain/src"),
            first.join("node_modules/dep/.git"),
            second.join("site/.git"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let backup = backup(
            &[first.to_str().unwrap(), second.to_str().unwrap()],
            &["build_artifacts", "git_repos"],
            &[],
        );

        let text = render_excludes_live(&backup).await;

        let (first, second) = (first.to_str().unwrap(), second.to_str().unwrap());
        for expected in [
            format!("{first}/app"),
            format!("{first}/nested/lib"),
            format!("{second}/site"),
        ] {
            assert!(lines(&text).contains(&expected.as_str()), "{text}");
        }
        assert!(
            !lines(&text).iter().any(|line| line.ends_with("/dep")),
            "{text}"
        );
        assert!(
            !lines(&text).iter().any(|line| line.ends_with("/plain")),
            "{text}"
        );
    }

    #[tokio::test]
    async fn the_live_render_without_git_repos_does_not_need_the_folders() {
        let backup = backup(&["/does/not/exist"], &["system_junk"], &[]);

        assert_eq!(
            render_excludes_live(&backup).await,
            render_excludes(&backup, &[])
        );
    }

    #[test]
    fn write_excludes_writes_only_when_the_content_changes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("backups/docs/excludes.txt");

        assert!(write_excludes(&path, "a\n").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\n");
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();

        assert!(!write_excludes(&path, "a\n").unwrap());
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );

        assert!(write_excludes(&path, "b\n").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b\n");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_excludes_file_is_an_error_not_a_rewrite() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("excludes.txt");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            return;
        }

        assert!(write_excludes(&path, "new").is_err());
    }
}
