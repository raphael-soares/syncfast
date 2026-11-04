//! Looking at a folder, local or remote, to decide what to sync. Only
//! metadata is read: names, sizes and dates. Never the content of a file.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;

use super::{OpError, Ops};
use crate::config::{expand_tilde, split_remote};
use crate::rclone::TreeEntry;

pub const DEFAULT_DEPTH: usize = 2;
pub const DEFAULT_MAX_FOLDERS: usize = 60;
const MAX_DEPTH: usize = 10;
const TOP_EXTENSIONS_OVERALL: usize = 10;
const TOP_EXTENSIONS_PER_FOLDER: usize = 3;
const NO_EXTENSION: &str = "(none)";
const LONGEST_EXTENSION: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExploreOptions {
    /// How many levels of folders to list below the target; deeper files
    /// still count in the folders above them.
    pub depth: usize,
    /// The most folders listed, so the answer fits in an agent's context.
    pub max_folders: usize,
}

impl Default for ExploreOptions {
    fn default() -> Self {
        Self {
            depth: DEFAULT_DEPTH,
            max_folders: DEFAULT_MAX_FOLDERS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtensionStat {
    /// Lowercase, without the dot; `(none)` for files without one.
    pub extension: String,
    pub files: u64,
    pub bytes: u64,
}

/// One folder of the tree. Counts include everything below it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderStat {
    /// Relative to the target; `.` for the target itself.
    pub path: String,
    pub depth: usize,
    pub files: u64,
    /// Files directly in this folder.
    pub direct_files: u64,
    /// Folders directly in this folder.
    pub subfolders: u64,
    pub bytes: u64,
    /// Modification time of the most recently changed file.
    pub newest: Option<Timestamp>,
    pub top_extensions: Vec<ExtensionStat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exploration {
    pub target: String,
    pub location: Location,
    pub depth: usize,
    pub files: u64,
    /// Folders at any depth below the target.
    pub folders: u64,
    pub bytes: u64,
    /// Files whose size the backend does not report (Google Docs); they
    /// count as 0 bytes.
    pub files_of_unknown_size: u64,
    pub newest: Option<Timestamp>,
    pub top_extensions: Vec<ExtensionStat>,
    /// The target and its folders down to `depth`, parents before their
    /// children and the biggest first among siblings.
    pub tree: Vec<FolderStat>,
    /// Folders within `depth` that are not in `tree` because of
    /// `max_folders`.
    pub folders_omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderList {
    pub path: String,
    pub folders: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FolderMade {
    pub path: String,
}

#[derive(Default)]
struct Agg {
    files: u64,
    direct_files: u64,
    subfolders: u64,
    bytes: u64,
    newest: Option<Timestamp>,
    extensions: HashMap<String, (u64, u64)>,
}

impl Agg {
    fn add_file(&mut self, size: u64, modified: Option<Timestamp>, extension: &str, direct: bool) {
        self.files += 1;
        self.direct_files += u64::from(direct);
        self.bytes += size;
        self.newest = self.newest.max(modified);
        let slot = self.extensions.entry(extension.to_owned()).or_default();
        slot.0 += 1;
        slot.1 += size;
    }

    fn top_extensions(&self, limit: usize) -> Vec<ExtensionStat> {
        let mut found: Vec<ExtensionStat> = self
            .extensions
            .iter()
            .map(|(extension, &(files, bytes))| ExtensionStat {
                extension: extension.clone(),
                files,
                bytes,
            })
            .collect();
        found.sort_by(|a, b| {
            b.files
                .cmp(&a.files)
                .then_with(|| a.extension.cmp(&b.extension))
        });
        found.truncate(limit);
        found
    }
}

fn extension_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => {
            let extension = name[dot + 1..].to_lowercase();
            if extension.chars().count() <= LONGEST_EXTENSION {
                extension
            } else {
                NO_EXTENSION.to_owned()
            }
        }
        _ => NO_EXTENSION.to_owned(),
    }
}

/// Aggregates a recursive listing. Only the folders down to `options.depth`
/// get their own entry; every file counts in all of its ancestors.
pub(crate) fn summarize(
    target: String,
    location: Location,
    entries: &[TreeEntry],
    options: ExploreOptions,
) -> Exploration {
    let depth = options.depth.min(MAX_DEPTH);
    let mut folders: HashMap<String, Agg> = HashMap::new();
    folders.entry(String::new()).or_default();
    let mut total_folders = 0_u64;
    let mut unknown_size = 0_u64;
    for entry in entries {
        let path = entry.path.trim_matches('/');
        let levels = path.matches('/').count() + 1;
        if entry.is_dir {
            total_folders += 1;
            if levels <= depth {
                folders.entry(path.to_owned()).or_default();
            }
            if levels - 1 <= depth {
                let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
                folders.entry(parent.to_owned()).or_default().subfolders += 1;
            }
            continue;
        }
        let size = u64::try_from(entry.size).unwrap_or_else(|_| {
            unknown_size += 1;
            0
        });
        let modified = entry
            .modified
            .as_deref()
            .and_then(|text| text.parse::<Timestamp>().ok());
        let extension = extension_of(path);
        let ancestor_ends = std::iter::once(0).chain(path.match_indices('/').map(|(at, _)| at));
        for (level, end) in ancestor_ends.enumerate().take(depth + 1) {
            let ancestor = &path[..end];
            let agg = match folders.get_mut(ancestor) {
                Some(agg) => agg,
                None => folders.entry(ancestor.to_owned()).or_default(),
            };
            agg.add_file(size, modified, &extension, level + 1 == levels);
        }
    }
    let root = folders.get("").map(|root| {
        (
            root.files,
            root.bytes,
            root.newest,
            root.top_extensions(TOP_EXTENSIONS_OVERALL),
        )
    });
    let (files, bytes, newest, top_extensions) = root.unwrap_or_default();
    let (tree, folders_omitted) = pick_tree(&folders, options.max_folders);
    Exploration {
        target,
        location,
        depth,
        files,
        folders: total_folders,
        bytes,
        files_of_unknown_size: unknown_size,
        newest,
        top_extensions,
        tree,
        folders_omitted,
    }
}

fn depth_of(path: &str) -> usize {
    if path.is_empty() {
        0
    } else {
        path.matches('/').count() + 1
    }
}

/// The folders to list: the target first, then level by level, the biggest
/// first, until `limit`; in the order a tree reads.
fn pick_tree(folders: &HashMap<String, Agg>, limit: usize) -> (Vec<FolderStat>, usize) {
    let mut ranked: Vec<(&String, &Agg)> = folders.iter().collect();
    ranked.sort_by(|(path_a, a), (path_b, b)| {
        depth_of(path_a)
            .cmp(&depth_of(path_b))
            .then_with(|| b.bytes.cmp(&a.bytes))
            .then_with(|| path_a.cmp(path_b))
    });
    let kept = limit.max(1).min(ranked.len());
    let omitted = ranked.len() - kept;
    ranked.truncate(kept);
    let mut children: BTreeMap<&str, Vec<(&String, &Agg)>> = BTreeMap::new();
    for &(path, agg) in &ranked {
        if !path.is_empty() {
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            children.entry(parent).or_default().push((path, agg));
        }
    }
    let mut tree = Vec::with_capacity(kept);
    let mut stack: Vec<(&String, &Agg)> = ranked
        .iter()
        .copied()
        .filter(|(path, _)| path.is_empty())
        .collect();
    while let Some((path, agg)) = stack.pop() {
        tree.push(FolderStat {
            path: if path.is_empty() {
                ".".to_owned()
            } else {
                path.clone()
            },
            depth: depth_of(path),
            files: agg.files,
            direct_files: agg.direct_files,
            subfolders: agg.subfolders,
            bytes: agg.bytes,
            newest: agg.newest,
            top_extensions: agg.top_extensions(TOP_EXTENSIONS_PER_FOLDER),
        });
        if let Some(below) = children.get(path.as_str()) {
            // `ranked` is ordered biggest first; the stack pops last first.
            stack.extend(below.iter().rev());
        }
    }
    (tree, omitted)
}

/// Where `target` points: `gdrive:Notes` is a remote path, anything else a
/// folder on this computer (`~` is the home folder; a relative path is
/// relative to `cwd`).
pub(super) fn classify(
    target: &str,
    home: &Path,
    cwd: &Path,
) -> Result<(String, Location), OpError> {
    let target = target.trim();
    if target.is_empty() {
        return Err(OpError::Invalid(
            "give a folder: a path on this computer or \"account:folder\"".to_owned(),
        ));
    }
    let local_looking = target.starts_with(['/', '~', '.']);
    if !local_looking && split_remote(target).is_some() {
        return Ok((target.to_owned(), Location::Remote));
    }
    let expanded = expand_tilde(Path::new(target), home);
    let absolute: PathBuf = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };
    let text = absolute
        .to_str()
        .ok_or_else(|| OpError::Invalid("the path is not valid text".to_owned()))?;
    Ok((text.to_owned(), Location::Local))
}

fn remote_path(path: &str) -> Result<&str, OpError> {
    let path = path.trim();
    if split_remote(path).is_none() || path.starts_with(['/', '~', '.']) {
        return Err(OpError::Invalid(
            "give a remote folder like \"gdrive:Notes\" (\"gdrive:\" is the whole account)"
                .to_owned(),
        ));
    }
    Ok(path)
}

impl Ops {
    /// Summarizes a folder, local or `account:folder`, from `rclone lsjson
    /// -R`: counts, sizes, common extensions and the newest date per
    /// folder, down to `options.depth` levels.
    ///
    /// # Errors
    /// Nothing to look at, or rclone cannot list the folder.
    pub async fn explore(
        &self,
        target: &str,
        options: ExploreOptions,
    ) -> Result<Exploration, OpError> {
        let cwd = std::env::current_dir()
            .map_err(|err| OpError::Failed(format!("cannot read the current folder: {err}")))?;
        let (path, location) = classify(target, &self.paths.home, &cwd)?;
        let entries = self.rclone.list_tree(&path).await?;
        Ok(summarize(path, location, &entries, options))
    }

    /// The names of the folders directly inside a remote folder.
    ///
    /// # Errors
    /// Not a remote path, or rclone cannot list it.
    pub async fn folders(&self, remote_path_text: &str) -> Result<FolderList, OpError> {
        let path = remote_path(remote_path_text)?;
        let mut folders = self.rclone.list_dirs(path).await?;
        folders.sort_by_key(|name| name.to_lowercase());
        Ok(FolderList {
            path: path.to_owned(),
            folders,
        })
    }

    /// Creates a remote folder and the ones above it that are missing.
    ///
    /// # Errors
    /// Not a remote path, or rclone cannot create it.
    pub async fn make_folder(&self, remote_path_text: &str) -> Result<FolderMade, OpError> {
        let path = remote_path(remote_path_text)?;
        self.rclone.mkdir(path).await?;
        Ok(FolderMade {
            path: path.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: i64, modified: &str) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            size,
            modified: Some(modified.to_owned()),
            is_dir: false,
        }
    }

    fn dir(path: &str) -> TreeEntry {
        TreeEntry {
            path: path.to_owned(),
            size: -1,
            modified: None,
            is_dir: true,
        }
    }

    fn options(depth: usize, max_folders: usize) -> ExploreOptions {
        ExploreOptions { depth, max_folders }
    }

    fn explored(entries: &[TreeEntry], options: ExploreOptions) -> Exploration {
        summarize("t".to_owned(), Location::Local, entries, options)
    }

    fn paths(found: &Exploration) -> Vec<&str> {
        found.tree.iter().map(|stat| stat.path.as_str()).collect()
    }

    #[test]
    fn files_below_the_depth_count_in_the_folders_that_are_listed() {
        let entries = [
            dir("a"),
            dir("a/b"),
            dir("a/b/c"),
            file("a/b/c/deep.txt", 10, "2026-01-02T00:00:00Z"),
            file("a/top.txt", 5, "2026-03-04T00:00:00Z"),
        ];

        let found = explored(&entries, options(1, 60));

        assert_eq!(paths(&found), [".", "a"]);
        assert_eq!((found.files, found.folders, found.bytes), (2, 3, 15));
        let a = &found.tree[1];
        assert_eq!(
            (a.files, a.direct_files, a.subfolders, a.bytes),
            (2, 1, 1, 15)
        );
        assert_eq!(
            a.newest.map(|at| at.to_string()).as_deref(),
            Some("2026-03-04T00:00:00Z")
        );
    }

    #[test]
    fn depth_zero_gives_only_the_totals_of_the_target() {
        let entries = [dir("a"), file("a/x.md", 7, "2026-01-01T00:00:00Z")];

        let found = explored(&entries, options(0, 60));

        assert_eq!(paths(&found), ["."]);
        assert_eq!((found.files, found.bytes), (1, 7));
    }

    #[test]
    fn the_biggest_folders_are_kept_first_and_the_rest_are_counted_as_omitted() {
        let entries = [
            dir("small"),
            dir("big"),
            dir("mid"),
            file("small/s.txt", 1, "2026-01-01T00:00:00Z"),
            file("big/b.txt", 900, "2026-01-01T00:00:00Z"),
            file("mid/m.txt", 50, "2026-01-01T00:00:00Z"),
        ];

        let found = explored(&entries, options(1, 3));

        assert_eq!(paths(&found), [".", "big", "mid"]);
        assert_eq!(found.folders_omitted, 1);
    }

    #[test]
    fn children_follow_their_parent_biggest_first() {
        let entries = [
            dir("a"),
            dir("b"),
            dir("a/x"),
            dir("a/y"),
            file("a/x/f", 1, "2026-01-01T00:00:00Z"),
            file("a/y/f", 20, "2026-01-01T00:00:00Z"),
            file("b/f", 100, "2026-01-01T00:00:00Z"),
        ];

        let found = explored(&entries, options(2, 60));

        assert_eq!(paths(&found), [".", "b", "a", "a/y", "a/x"]);
    }

    #[test]
    fn extensions_are_lowercase_counted_and_files_without_one_are_none() {
        let entries = [
            file("A.PNG", 1, "2026-01-01T00:00:00Z"),
            file("b.png", 1, "2026-01-01T00:00:00Z"),
            file("c.txt", 1, "2026-01-01T00:00:00Z"),
            file("Makefile", 1, "2026-01-01T00:00:00Z"),
            file(".env", 1, "2026-01-01T00:00:00Z"),
            file("archive.tar.gz", 1, "2026-01-01T00:00:00Z"),
        ];

        let found = explored(&entries, options(0, 60));

        let counted: Vec<(&str, u64)> = found
            .top_extensions
            .iter()
            .map(|ext| (ext.extension.as_str(), ext.files))
            .collect();
        assert_eq!(counted, [("(none)", 2), ("png", 2), ("gz", 1), ("txt", 1)]);
    }

    #[test]
    fn a_file_of_unknown_size_counts_as_zero_bytes_and_is_reported() {
        let entries = [
            file("doc", -1, "2026-01-01T00:00:00Z"),
            file("x", 4, "2026-01-01T00:00:00Z"),
        ];

        let found = explored(&entries, options(0, 60));

        assert_eq!((found.bytes, found.files_of_unknown_size), (4, 1));
    }

    #[test]
    fn a_remote_path_is_told_from_a_local_one() {
        let home = Path::new("/home/ada");
        let cwd = Path::new("/work");
        let classify = |target: &str| classify(target, home, cwd).unwrap();

        assert_eq!(
            classify("gdrive:Notes"),
            ("gdrive:Notes".to_owned(), Location::Remote)
        );
        assert_eq!(
            classify("gdrive:"),
            ("gdrive:".to_owned(), Location::Remote)
        );
        assert_eq!(
            classify("~/Notes"),
            ("/home/ada/Notes".to_owned(), Location::Local)
        );
        assert_eq!(
            classify("/srv/data"),
            ("/srv/data".to_owned(), Location::Local)
        );
        assert_eq!(
            classify("./a:b"),
            ("/work/./a:b".to_owned(), Location::Local)
        );
        assert_eq!(classify("docs"), ("/work/docs".to_owned(), Location::Local));
        assert!(matches!(classify_err(""), OpError::Invalid(_)));
    }

    fn classify_err(target: &str) -> OpError {
        classify(target, Path::new("/h"), Path::new("/w")).unwrap_err()
    }

    #[test]
    fn only_a_remote_path_is_taken_by_the_remote_commands() {
        assert_eq!(remote_path(" gdrive:Notes ").unwrap(), "gdrive:Notes");
        assert!(remote_path("/etc").is_err());
        assert!(remote_path("Notes").is_err());
        assert!(remote_path("./a:b").is_err());
    }
}
