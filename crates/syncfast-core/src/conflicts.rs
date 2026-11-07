//! Finds the files rclone leaves for the loser of a conflict
//! (`<name>.conflict<N>`, or `<name>.path1` and `<name>.path2` with
//! `--conflict-loser pathname`), and settles them: keep the current file,
//! keep the conflicted copy, or keep both.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use jiff::{Timestamp, Zoned};

use crate::behavior::ConflictLoser;
use crate::bisync::stamp;
use crate::filters::DirMatcher;

const MARKER: &str = ".conflict";
const PATH_MARKER: &str = ".path";

/// The names a sync's conflict files can have. `.conflict<N>` is always
/// recognised, since an earlier setting may have left some; `.path1` and
/// `.path2` only when the sync names its losers by path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    Num,
    Pathname,
}

impl From<ConflictLoser> for Naming {
    fn from(loser: ConflictLoser) -> Self {
        match loser {
            ConflictLoser::Pathname => Self::Pathname,
            ConflictLoser::Num | ConflictLoser::Delete => Self::Num,
        }
    }
}

/// Splits `<original>.conflict<N>` (or `<original>.path<1|2>` with
/// [`Naming::Pathname`]) into `<original>` and the number. The original
/// name must not be empty.
fn split_conflict_name(name: &str, naming: Naming) -> Option<(&str, &str)> {
    let numbered = name.rsplit_once(MARKER).filter(|(original, number)| {
        !original.is_empty()
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    });
    let by_path = || {
        name.rsplit_once(PATH_MARKER)
            .filter(|(original, side)| !original.is_empty() && matches!(*side, "1" | "2"))
    };
    numbered.or_else(|| (naming == Naming::Pathname).then(by_path).flatten())
}

/// True for a conflict file of `naming` with a non-empty original name.
#[must_use]
pub fn is_conflict_name(name: &str, naming: Naming) -> bool {
    split_conflict_name(name, naming).is_some()
}

/// Paths relative to `root`, sorted, of every conflict file in the tree.
/// Directories excluded by `matcher` are not entered, and symlinks are not
/// followed.
///
/// # Errors
/// A directory cannot be read.
pub fn find_conflicts(
    root: &Path,
    matcher: &DirMatcher,
    naming: Naming,
) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(rel_dir) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&rel_dir))? {
            let entry = entry?;
            let rel = rel_dir.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() {
                if !matcher.is_excluded(&rel) {
                    pending.push(rel);
                }
            } else if kind.is_file()
                && is_conflict_name(&entry.file_name().to_string_lossy(), naming)
            {
                found.push(rel);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// What the user chose for one conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Delete the conflicted copy.
    KeepCurrent,
    /// The conflicted copy becomes the file; the current one goes to the
    /// versions folder.
    KeepConflicted,
    /// The conflicted copy stays beside the file under a new name.
    KeepBoth,
}

/// Splits at the last `.`, except when there is none or it is the first
/// character (a dotfile): then the extension is empty.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(index) if index > 0 => name.split_at(index),
        _ => (name, ""),
    }
}

/// The name `Keep Both` gives the conflicted copy of `original`:
/// `<stem> (conflict <number>)<.ext>`, and ` 2`, ` 3`... before the
/// extension while `taken` says the name is in use.
#[must_use]
pub fn keep_both_name(original: &str, number: &str, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_extension(original);
    let base = format!("{stem} (conflict {number})");
    let mut name = format!("{base}{ext}");
    let mut attempt = 2_u32;
    while taken(&name) {
        name = format!("{base} {attempt}{ext}");
        attempt += 1;
    }
    name
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    pub size: u64,
    pub modified: Option<Timestamp>,
}

fn facts(path: &Path) -> io::Result<FileFacts> {
    let meta = std::fs::metadata(path)?;
    Ok(FileFacts {
        size: meta.len(),
        modified: meta
            .modified()
            .ok()
            .and_then(|time: SystemTime| Timestamp::try_from(time).ok()),
    })
}

/// One conflict as the Conflicts tab lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The conflict file, relative to the sync's folder.
    pub path: PathBuf,
    /// The path without the conflict suffix.
    pub original: PathBuf,
    /// `None` when the original no longer exists.
    pub current: Option<FileFacts>,
    pub conflicted: FileFacts,
}

/// [`find_conflicts`] with the size and modification time of both files. A
/// conflict that disappears while listing is left out.
///
/// # Errors
/// A directory or a file cannot be read.
pub fn list_conflicts(
    root: &Path,
    matcher: &DirMatcher,
    naming: Naming,
) -> io::Result<Vec<Conflict>> {
    let mut conflicts = Vec::new();
    for path in find_conflicts(root, matcher, naming)? {
        let Some(original) = original_path(&path, naming) else {
            continue;
        };
        let conflicted = match facts(&root.join(&path)) {
            Ok(facts) => facts,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        let current = match facts(&root.join(&original)) {
            Ok(facts) => Some(facts),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        conflicts.push(Conflict {
            path,
            original,
            current,
            conflicted,
        });
    }
    Ok(conflicts)
}

/// `a/b.txt.conflict1` gives `a/b.txt`.
fn original_path(conflict: &Path, naming: Naming) -> Option<PathBuf> {
    let name = conflict.file_name()?.to_str()?;
    let (original, _) = split_conflict_name(name, naming)?;
    Some(conflict.with_file_name(original))
}

fn occupied(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// What `from` is, read once: a symlink is kept as a link (its target may
/// be gone, or outside the folder), anything else as content.
enum Original {
    Link(PathBuf),
    File(std::fs::File),
}

impl Original {
    fn open(from: &Path) -> io::Result<Self> {
        if std::fs::symlink_metadata(from)?.file_type().is_symlink() {
            std::fs::read_link(from).map(Self::Link)
        } else {
            std::fs::File::open(from).map(Self::File)
        }
    }

    /// Creates `candidate` as a copy; `AlreadyExists` when it is taken.
    fn place(&mut self, candidate: &Path) -> io::Result<()> {
        match self {
            Self::Link(target) => std::os::unix::fs::symlink(target, candidate),
            Self::File(source) => {
                let mut target = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(candidate)?;
                io::copy(source, &mut target)?;
                target.set_permissions(source.metadata()?.permissions())
            }
        }
    }
}

/// Copies `from` to `to`, creating the destination's folders, and never over
/// an existing file: when `to` is taken the copy gets ` 2`, ` 3`... before
/// the extension. A symlink is copied as a symlink. Returns where it went.
fn copy_without_overwriting(from: &Path, to: &Path) -> io::Result<PathBuf> {
    let (Some(dir), Some(name)) = (to.parent(), to.file_name().and_then(|name| name.to_str()))
    else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no file name"));
    };
    std::fs::create_dir_all(dir)?;
    let (stem, ext) = split_extension(name);
    let mut original = Original::open(from)?;
    let mut attempt = 1_u32;
    loop {
        let candidate = if attempt == 1 {
            dir.join(name)
        } else {
            dir.join(format!("{stem} {attempt}{ext}"))
        };
        match original.place(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
            Err(err) => return Err(err),
        }
    }
}

/// Settles the conflict `conflict` (a path relative to `root`, as
/// [`find_conflicts`] returns it). `Keep Conflicted Copy` first copies the
/// current file to `<versions>/<AAAAMMDD-HHMMSS>/<relative path>`, the layout
/// of a round's `--backup-dir1`, so the old content stays for the retention
/// period, then renames the conflicted copy over the file in one step: the
/// file is never missing and a failed copy leaves everything as it was.
///
/// # Errors
/// `conflict` is not a relative conflict file of `naming` under `root`, or a
/// file cannot be moved or deleted.
pub fn resolve(
    root: &Path,
    conflict: &Path,
    naming: Naming,
    resolution: Resolution,
    versions: &Path,
    now: &Zoned,
) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "not a conflict file");
    if !conflict
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(invalid());
    }
    let name = conflict
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid)?;
    let (original, number) = split_conflict_name(name, naming).ok_or_else(invalid)?;
    let from = root.join(conflict);
    let dir = from.parent().ok_or_else(invalid)?;
    let current = dir.join(original);
    match resolution {
        Resolution::KeepCurrent => std::fs::remove_file(&from),
        Resolution::KeepConflicted => {
            if occupied(&current) {
                let kept = versions
                    .join(stamp(now))
                    .join(conflict.with_file_name(original));
                copy_without_overwriting(&current, &kept)?;
            }
            std::fs::rename(&from, &current)
        }
        Resolution::KeepBoth => {
            let renamed = keep_both_name(original, number, |name| occupied(&dir.join(name)));
            std::fs::rename(&from, dir.join(renamed))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_names_need_an_original_name_and_a_number() {
        assert!(is_conflict_name("a.txt.conflict1", Naming::Num));
        assert!(is_conflict_name("a.txt.conflict12", Naming::Num));
        assert!(is_conflict_name("a.conflict2.conflict3", Naming::Num));
        assert!(!is_conflict_name("a.txt.conflict", Naming::Num));
        assert!(!is_conflict_name("a.txt.conflictx", Naming::Num));
        assert!(!is_conflict_name(".conflict1", Naming::Num));
        assert!(!is_conflict_name("a.txt", Naming::Num));
    }

    #[test]
    fn find_conflicts_walks_the_tree_and_skips_excluded_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for path in [
            "top.txt.conflict1",
            "notes/deep/a.md.conflict2",
            "notes/b.md",
            "node_modules/pkg/x.js.conflict1",
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "x").unwrap();
        }
        let matcher = DirMatcher::new("- node_modules/\n");

        let found = find_conflicts(root, &matcher, Naming::Num).unwrap();

        assert_eq!(
            found,
            vec![
                PathBuf::from("notes/deep/a.md.conflict2"),
                PathBuf::from("top.txt.conflict1"),
            ]
        );
    }

    #[test]
    fn find_conflicts_does_not_follow_symlinked_directories() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("a.txt.conflict1"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        let found = find_conflicts(dir.path(), &DirMatcher::new(""), Naming::Num).unwrap();

        assert!(found.is_empty());
    }

    fn free(_: &str) -> bool {
        false
    }

    #[test]
    fn keep_both_puts_the_number_before_the_extension() {
        assert_eq!(keep_both_name("a.txt", "1", free), "a (conflict 1).txt");
    }

    #[test]
    fn keep_both_without_extension_appends_the_number() {
        assert_eq!(
            keep_both_name("Makefile", "2", free),
            "Makefile (conflict 2)"
        );
    }

    #[test]
    fn keep_both_treats_a_lone_leading_dot_as_no_extension() {
        assert_eq!(keep_both_name(".bashrc", "1", free), ".bashrc (conflict 1)");
    }

    #[test]
    fn keep_both_keeps_a_leading_dot_stem_when_there_is_a_later_dot() {
        assert_eq!(
            keep_both_name(".config.json", "1", free),
            ".config (conflict 1).json"
        );
    }

    #[test]
    fn keep_both_takes_only_the_last_suffix_as_extension() {
        assert_eq!(
            keep_both_name("a.tar.gz", "3", free),
            "a.tar (conflict 3).gz"
        );
    }

    #[test]
    fn keep_both_counts_up_from_two_while_the_name_is_taken() {
        let taken = ["a (conflict 1).txt", "a (conflict 1) 2.txt"];

        let name = keep_both_name("a.txt", "1", |candidate| taken.contains(&candidate));

        assert_eq!(name, "a (conflict 1) 3.txt");
    }

    fn put(root: &Path, rel: &str, text: &str) {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }

    fn at(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    #[test]
    fn list_conflicts_reports_both_files_and_a_missing_current() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "n/a.md", "current");
        put(dir.path(), "n/a.md.conflict1", "old");
        put(dir.path(), "gone.txt.conflict2", "lost");

        let found = list_conflicts(dir.path(), &DirMatcher::new(""), Naming::Num).unwrap();

        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, PathBuf::from("gone.txt.conflict2"));
        assert_eq!(found[0].original, PathBuf::from("gone.txt"));
        assert_eq!(found[0].current, None);
        assert_eq!(found[0].conflicted.size, 4);
        assert_eq!(found[1].original, PathBuf::from("n/a.md"));
        assert_eq!(found[1].current.as_ref().map(|facts| facts.size), Some(7));
        assert_eq!(found[1].conflicted.size, 3);
        assert!(found[1].conflicted.modified.is_some());
    }

    #[test]
    fn keep_current_deletes_only_the_conflicted_copy() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "n/a.md", "current");
        put(dir.path(), "n/a.md.conflict1", "old");

        resolve(
            dir.path(),
            Path::new("n/a.md.conflict1"),
            Naming::Num,
            Resolution::KeepCurrent,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        assert_eq!(read(dir.path(), "n/a.md"), "current");
        assert!(!dir.path().join("n/a.md.conflict1").exists());
    }

    #[test]
    fn keep_conflicted_moves_the_current_file_to_a_dated_versions_folder() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "n/a.md", "current");
        put(dir.path(), "n/a.md.conflict1", "old");

        resolve(
            dir.path(),
            Path::new("n/a.md.conflict1"),
            Naming::Num,
            Resolution::KeepConflicted,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        assert_eq!(read(dir.path(), "n/a.md"), "old");
        assert!(!dir.path().join("n/a.md.conflict1").exists());
        assert_eq!(read(versions.path(), "20260102-150405/n/a.md"), "current");
    }

    #[test]
    fn keep_conflicted_keeps_a_symlinked_current_file_as_a_link_in_versions() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "target.txt", "target content");
        std::os::unix::fs::symlink("target.txt", dir.path().join("a.md")).unwrap();
        put(dir.path(), "a.md.conflict1", "old");

        resolve(
            dir.path(),
            Path::new("a.md.conflict1"),
            Naming::Num,
            Resolution::KeepConflicted,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        let kept = versions.path().join("20260102-150405/a.md");
        assert!(
            std::fs::symlink_metadata(&kept)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_link(&kept).unwrap(), Path::new("target.txt"));
        assert_eq!(read(dir.path(), "a.md"), "old");
        assert!(
            !std::fs::symlink_metadata(dir.path().join("a.md"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(read(dir.path(), "target.txt"), "target content");
    }

    #[test]
    fn keep_conflicted_keeps_a_dangling_symlink_too() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("gone.txt", dir.path().join("a.md")).unwrap();
        put(dir.path(), "a.md.conflict1", "old");

        resolve(
            dir.path(),
            Path::new("a.md.conflict1"),
            Naming::Num,
            Resolution::KeepConflicted,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        let kept = versions.path().join("20260102-150405/a.md");
        assert_eq!(std::fs::read_link(kept).unwrap(), Path::new("gone.txt"));
        assert_eq!(read(dir.path(), "a.md"), "old");
    }

    #[test]
    fn keep_conflicted_without_a_current_file_only_renames() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "a.md.conflict1", "old");

        resolve(
            dir.path(),
            Path::new("a.md.conflict1"),
            Naming::Num,
            Resolution::KeepConflicted,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        assert_eq!(read(dir.path(), "a.md"), "old");
        assert_eq!(std::fs::read_dir(versions.path()).unwrap().count(), 0);
    }

    #[test]
    fn keep_both_renames_the_copy_and_avoids_a_name_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "n/a.md", "current");
        put(dir.path(), "n/a.md.conflict1", "old");
        put(dir.path(), "n/a (conflict 1).md", "taken");

        resolve(
            dir.path(),
            Path::new("n/a.md.conflict1"),
            Naming::Num,
            Resolution::KeepBoth,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        assert_eq!(read(dir.path(), "n/a.md"), "current");
        assert_eq!(read(dir.path(), "n/a (conflict 1).md"), "taken");
        assert_eq!(read(dir.path(), "n/a (conflict 1) 2.md"), "old");
        assert!(!dir.path().join("n/a.md.conflict1").exists());
    }

    #[test]
    fn resolve_refuses_paths_that_leave_the_folder_or_are_not_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "a.md", "current");
        let now = at("2026-01-02T15:04:05[UTC]");

        for bad in ["../a.md.conflict1", "/etc/a.md.conflict1", "a.md"] {
            let err = resolve(
                dir.path(),
                Path::new(bad),
                Naming::Num,
                Resolution::KeepCurrent,
                versions.path(),
                &now,
            )
            .unwrap_err();

            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{bad}");
        }
        assert_eq!(read(dir.path(), "a.md"), "current");
    }

    #[test]
    fn keep_conflicted_moves_across_file_systems_by_copying() {
        let shm = Path::new("/dev/shm");
        let dir = tempfile::tempdir().unwrap();
        let Ok(versions) = tempfile::tempdir_in(shm) else {
            return;
        };
        put(dir.path(), "a.md", "current");
        put(dir.path(), "a.md.conflict1", "old");

        resolve(
            dir.path(),
            Path::new("a.md.conflict1"),
            Naming::Num,
            Resolution::KeepConflicted,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        assert_eq!(read(dir.path(), "a.md"), "old");
        assert_eq!(read(versions.path(), "20260102-150405/a.md"), "current");
    }

    #[test]
    fn two_conflicted_copies_kept_in_one_second_preserve_both_older_versions() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "a.md", "first");
        put(dir.path(), "a.md.conflict1", "second");
        put(dir.path(), "a.md.conflict2", "third");
        let now = at("2026-01-02T15:04:05[UTC]");

        for conflict in ["a.md.conflict1", "a.md.conflict2"] {
            resolve(
                dir.path(),
                Path::new(conflict),
                Naming::Num,
                Resolution::KeepConflicted,
                versions.path(),
                &now,
            )
            .unwrap();
        }

        assert_eq!(read(dir.path(), "a.md"), "third");
        assert_eq!(read(versions.path(), "20260102-150405/a.md"), "first");
        assert_eq!(read(versions.path(), "20260102-150405/a 2.md"), "second");
    }

    #[test]
    fn conflict_number_names_are_recognised_whatever_the_naming() {
        for naming in [Naming::Num, Naming::Pathname] {
            assert!(is_conflict_name("a.txt.conflict3", naming));
        }
    }

    #[test]
    fn path_names_are_recognised_only_when_the_naming_is_pathname() {
        for name in ["a.txt.path1", "a.txt.path2"] {
            assert!(!is_conflict_name(name, Naming::Num), "{name}");
            assert!(is_conflict_name(name, Naming::Pathname), "{name}");
        }
        for name in ["a.txt.path3", "a.txt.path", ".path1", "a.txt.path12"] {
            assert!(!is_conflict_name(name, Naming::Pathname), "{name}");
        }
    }

    #[test]
    fn the_naming_follows_the_conflict_loser() {
        assert_eq!(Naming::from(ConflictLoser::Num), Naming::Num);
        assert_eq!(Naming::from(ConflictLoser::Pathname), Naming::Pathname);
        assert_eq!(Naming::from(ConflictLoser::Delete), Naming::Num);
    }

    #[test]
    fn find_conflicts_lists_path_files_only_for_a_pathname_sync() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "a.txt.path1", "x");
        put(dir.path(), "n/b.txt.path2", "x");
        put(dir.path(), "c.txt.conflict3", "x");
        let matcher = DirMatcher::new("");

        let by_number = find_conflicts(dir.path(), &matcher, Naming::Num).unwrap();
        let by_path = find_conflicts(dir.path(), &matcher, Naming::Pathname).unwrap();

        assert_eq!(by_number, vec![PathBuf::from("c.txt.conflict3")]);
        assert_eq!(
            by_path,
            vec![
                PathBuf::from("a.txt.path1"),
                PathBuf::from("c.txt.conflict3"),
                PathBuf::from("n/b.txt.path2"),
            ]
        );
    }

    #[test]
    fn list_conflicts_of_a_pathname_sync_pairs_each_path_file_with_its_original() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), "n/a.md", "current");
        put(dir.path(), "n/a.md.path1", "old");

        let found = list_conflicts(dir.path(), &DirMatcher::new(""), Naming::Pathname).unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].original, PathBuf::from("n/a.md"));
        assert_eq!(found[0].current.as_ref().map(|facts| facts.size), Some(7));
    }

    #[test]
    fn keep_both_names_a_path_file_after_the_digit_of_its_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "a.md", "current");
        put(dir.path(), "a.md.path2", "other");

        resolve(
            dir.path(),
            Path::new("a.md.path2"),
            Naming::Pathname,
            Resolution::KeepBoth,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap();

        assert_eq!(read(dir.path(), "a (conflict 2).md"), "other");
        assert_eq!(read(dir.path(), "a.md"), "current");
        assert!(!dir.path().join("a.md.path2").exists());
    }

    #[test]
    fn resolve_refuses_a_path_file_when_the_sync_does_not_name_losers_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        put(dir.path(), "a.md.path1", "x");

        let err = resolve(
            dir.path(),
            Path::new("a.md.path1"),
            Naming::Num,
            Resolution::KeepCurrent,
            versions.path(),
            &at("2026-01-02T15:04:05[UTC]"),
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
