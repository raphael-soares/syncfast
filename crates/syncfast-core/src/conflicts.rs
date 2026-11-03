//! Finds the `<name>.conflict<N>` files rclone leaves for the loser of a
//! conflict (`--conflict-loser num`).

use std::io;
use std::path::{Path, PathBuf};

use crate::filters::DirMatcher;

const MARKER: &str = ".conflict";

/// True for `<original>.conflict<N>` with a non-empty `<original>`.
#[must_use]
pub fn is_conflict_name(name: &str) -> bool {
    name.rsplit_once(MARKER).is_some_and(|(original, number)| {
        !original.is_empty()
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// Paths relative to `root`, sorted, of every conflict file in the tree.
/// Directories excluded by `matcher` are not entered, and symlinks are not
/// followed.
///
/// # Errors
/// A directory cannot be read.
pub fn find_conflicts(root: &Path, matcher: &DirMatcher) -> io::Result<Vec<PathBuf>> {
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
            } else if kind.is_file() && is_conflict_name(&entry.file_name().to_string_lossy()) {
                found.push(rel);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_names_need_an_original_name_and_a_number() {
        assert!(is_conflict_name("a.txt.conflict1"));
        assert!(is_conflict_name("a.txt.conflict12"));
        assert!(is_conflict_name("a.conflict2.conflict3"));
        assert!(!is_conflict_name("a.txt.conflict"));
        assert!(!is_conflict_name("a.txt.conflictx"));
        assert!(!is_conflict_name(".conflict1"));
        assert!(!is_conflict_name("a.txt"));
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

        let found = find_conflicts(root, &matcher).unwrap();

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

        let found = find_conflicts(dir.path(), &DirMatcher::new("")).unwrap();

        assert!(found.is_empty());
    }
}
