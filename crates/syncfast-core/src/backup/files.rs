//! The files inside a snapshot, one folder at a time (spec phase 3, step
//! 9): what the Browse Files sheet shows.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::backup::key::Key;
use crate::backup::snapshots::SnapshotView;
use crate::restic::{Repository, Restic, ResticError};

/// Entries of one folder that are read; a folder with more is cut and says
/// so.
pub const LIST_LIMIT: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl NodeKind {
    fn of(restic_type: &str) -> Self {
        match restic_type {
            "file" => Self::File,
            "dir" => Self::Dir,
            "symlink" => Self::Symlink,
            _ => Self::Other,
        }
    }
}

/// A file, folder or link of a snapshot. `path` is absolute inside the
/// snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NodeView {
    pub name: String,
    pub kind: NodeKind,
    pub path: String,
    pub size: Option<u64>,
    pub mtime: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Listing {
    pub nodes: Vec<NodeView>,
    /// The folder has more than [`LIST_LIMIT`] entries; the rest is not in
    /// `nodes`.
    pub truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ListError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Restic(#[from] ResticError),
}

#[derive(Deserialize)]
struct Line {
    struct_type: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    path: String,
    size: Option<u64>,
    mtime: Option<Timestamp>,
}

/// The nodes `restic ls --json` printed for `dir`, folders first, each
/// group by name. The snapshot's own line and the folder itself are not
/// entries.
///
/// # Errors
/// A line is not JSON.
pub fn parse_listing(stdout: &[u8], dir: &str) -> Result<Listing, ResticError> {
    let dir = dir.trim_end_matches('/');
    let mut nodes = Vec::new();
    let mut truncated = false;
    for line in String::from_utf8_lossy(stdout).lines() {
        if line.trim().is_empty() {
            continue;
        }
        let line: Line = serde_json::from_str(line).map_err(|_| ResticError::BadOutput("ls"))?;
        if line.struct_type.as_deref() != Some("node") || line.path.trim_end_matches('/') == dir {
            continue;
        }
        if nodes.len() == LIST_LIMIT {
            truncated = true;
            break;
        }
        nodes.push(NodeView {
            name: line.name,
            kind: NodeKind::of(&line.kind),
            path: line.path,
            size: line.size,
            mtime: line.mtime,
        });
    }
    nodes.sort_by_cached_key(|node| (node.kind != NodeKind::Dir, node.name.to_lowercase()));
    Ok(Listing { nodes, truncated })
}

/// The folder or file a backup root is, by its own line of `ls`.
fn root_of(stdout: &[u8], root: &str) -> Result<Option<NodeView>, ResticError> {
    for line in String::from_utf8_lossy(stdout).lines() {
        if line.trim().is_empty() {
            continue;
        }
        let line: Line = serde_json::from_str(line).map_err(|_| ResticError::BadOutput("ls"))?;
        if line.struct_type.as_deref() == Some("node") && line.path == root {
            return Ok(Some(NodeView {
                name: line.path,
                kind: NodeKind::of(&line.kind),
                path: root.to_owned(),
                size: line.size,
                mtime: line.mtime,
            }));
        }
    }
    Ok(None)
}

fn check_absolute(dir: &str) -> Result<(), ListError> {
    if dir.starts_with('/') {
        Ok(())
    } else {
        Err(ListError::Invalid(format!(
            "\"{dir}\" is not an absolute path of the snapshot."
        )))
    }
}

/// What is in `dir` of `snapshot`; an empty `dir` lists the folders the
/// backup was made of, each under its absolute path.
///
/// # Errors
/// `dir` is not an absolute path of the snapshot, or restic fails.
pub async fn list_files(
    restic: &Restic,
    repository: &Repository,
    key: &Key,
    snapshot: &SnapshotView,
    dir: &str,
) -> Result<Listing, ListError> {
    if dir.is_empty() {
        let mut nodes = Vec::new();
        for root in &snapshot.paths {
            let output = restic
                .run(
                    repository,
                    key,
                    &["ls", "--json", &snapshot.id, root],
                    false,
                )
                .await?;
            nodes.extend(root_of(&output.stdout, root)?);
        }
        return Ok(Listing {
            nodes,
            truncated: false,
        });
    }
    check_absolute(dir)?;
    let output = restic
        .run(repository, key, &["ls", "--json", &snapshot.id, dir], false)
        .await?;
    Ok(parse_listing(&output.stdout, dir)?)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/restic-0.19.1")
                .join(name),
        )
        .unwrap()
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect()
    }

    #[test]
    fn a_listing_has_the_children_and_not_the_folder_or_the_snapshot() {
        let listing = parse_listing(&fixture("ls_dir.out"), "/tmp/fixture/src/docs").unwrap();

        assert_eq!(names(&listing), ["sub", "a.txt", "notes.log"]);
        assert!(!listing.truncated);
    }

    #[test]
    fn a_node_carries_its_kind_path_size_and_time() {
        let listing = parse_listing(&fixture("ls_dir.out"), "/tmp/fixture/src/docs").unwrap();

        let file = &listing.nodes[1];
        assert_eq!(file.kind, NodeKind::File);
        assert_eq!(file.path, "/tmp/fixture/src/docs/a.txt");
        assert_eq!(file.size, Some(6));
        assert_eq!(
            file.mtime.map(|time| time.to_string()),
            Some("2026-11-07T23:58:27.55524787Z".to_owned())
        );
        assert_eq!(listing.nodes[0].kind, NodeKind::Dir);
        assert_eq!(listing.nodes[0].size, None);
    }

    #[test]
    fn only_an_absolute_path_of_the_snapshot_can_be_listed() {
        assert!(check_absolute("/home/ana").is_ok());
        let err = check_absolute("home/ana").unwrap_err();
        assert!(matches!(&err, ListError::Invalid(text) if text.contains("home/ana")));
    }

    #[test]
    fn a_trailing_slash_on_the_folder_still_drops_it() {
        let listing = parse_listing(&fixture("ls_dir.out"), "/tmp/fixture/src/docs/").unwrap();

        assert_eq!(listing.nodes.len(), 3);
    }

    #[test]
    fn a_recursive_listing_keeps_every_level_it_is_given() {
        let listing = parse_listing(&fixture("ls_recursive.out"), "/tmp/fixture/src/docs").unwrap();

        assert!(
            listing
                .nodes
                .iter()
                .any(|node| node.path.ends_with("/sub/b.txt"))
        );
        assert!(
            listing
                .nodes
                .iter()
                .all(|node| node.path != "/tmp/fixture/src/docs")
        );
    }

    fn many(count: usize) -> Vec<u8> {
        let mut text = String::from("{\"struct_type\":\"snapshot\"}\n");
        for index in 0..count {
            writeln!(
                text,
                "{{\"name\":\"f{index}\",\"type\":\"file\",\"path\":\"/d/f{index}\",\"size\":1,\"struct_type\":\"node\"}}"
            )
            .unwrap();
        }
        text.into_bytes()
    }

    #[test]
    fn a_folder_at_the_limit_is_whole() {
        let listing = parse_listing(&many(LIST_LIMIT), "/d").unwrap();

        assert_eq!(listing.nodes.len(), LIST_LIMIT);
        assert!(!listing.truncated);
    }

    #[test]
    fn a_folder_over_the_limit_is_cut_and_says_so() {
        let listing = parse_listing(&many(LIST_LIMIT + 1), "/d").unwrap();

        assert_eq!(listing.nodes.len(), LIST_LIMIT);
        assert!(listing.truncated);
    }

    #[test]
    fn folders_come_first_and_names_sort_without_regard_to_case() {
        let text = concat!(
            "{\"name\":\"b\",\"type\":\"file\",\"path\":\"/d/b\",\"struct_type\":\"node\"}\n",
            "{\"name\":\"Z\",\"type\":\"dir\",\"path\":\"/d/Z\",\"struct_type\":\"node\"}\n",
            "{\"name\":\"A\",\"type\":\"file\",\"path\":\"/d/A\",\"struct_type\":\"node\"}\n",
            "{\"name\":\"c\",\"type\":\"symlink\",\"path\":\"/d/c\",\"struct_type\":\"node\"}\n",
            "{\"name\":\"s\",\"type\":\"socket\",\"path\":\"/d/s\",\"struct_type\":\"node\"}\n",
        );

        let listing = parse_listing(text.as_bytes(), "/d").unwrap();

        assert_eq!(names(&listing), ["Z", "A", "b", "c", "s"]);
        assert_eq!(listing.nodes[3].kind, NodeKind::Symlink);
        assert_eq!(listing.nodes[4].kind, NodeKind::Other);
    }

    #[test]
    fn output_that_is_not_json_is_an_error() {
        assert!(matches!(
            parse_listing(b"not json\n", "/d"),
            Err(ResticError::BadOutput(_))
        ));
    }

    #[test]
    fn a_backup_root_is_named_by_its_whole_path() {
        let root = root_of(&fixture("ls_dir.out"), "/tmp/fixture/src/docs")
            .unwrap()
            .unwrap();

        assert_eq!(root.name, "/tmp/fixture/src/docs");
        assert_eq!(root.kind, NodeKind::Dir);
        assert_eq!(root.path, "/tmp/fixture/src/docs");
    }

    #[test]
    fn a_root_that_is_not_in_the_output_gives_nothing() {
        assert_eq!(root_of(&fixture("ls_dir.out"), "/elsewhere").unwrap(), None);
    }

    #[test]
    fn blank_lines_in_the_output_are_skipped_when_looking_for_the_root() {
        let mut out = b"\n\n".to_vec();
        out.extend(fixture("ls_dir.out"));

        let root = root_of(&out, "/tmp/fixture/src/docs").unwrap().unwrap();

        assert_eq!(root.path, "/tmp/fixture/src/docs");
    }
}
