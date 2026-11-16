//! What to do with each Google file, from what was tracked, what is on disk
//! and what is on Drive. Pure: no I/O (ADR 0010).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::link::{
    LocalLink, drive_name, file_name, kind_of_mime, render, split_link_name, untitled,
};
use super::remote::RemoteFile;
use super::state::{Link, Tracked};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteKind {
    New,
    /// Same path, new text.
    Refresh,
    /// Drive renamed or moved the file; the old link is removed.
    Renamed {
        from: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Trash {
        id: String,
        path: String,
    },
    Move {
        id: String,
        name: String,
        folder: String,
        from: String,
        to: String,
    },
    /// `foreign`: the source is not a file of this sync, and may not be one
    /// the account can open.
    Copy {
        source: String,
        name: String,
        folder: String,
        path: String,
        foreign: bool,
    },
    Write {
        id: String,
        path: String,
        text: String,
        kind: WriteKind,
    },
    /// `gone`: the file is no longer on Drive.
    Remove {
        id: Option<String>,
        path: String,
        gone: bool,
    },
    KeepEdited {
        id: Option<String>,
        path: String,
    },
    /// Renames `from` to `<to>.conflict<N>`.
    ConflictCopy {
        id: String,
        from: String,
        to: String,
    },
    Warn {
        path: String,
    },
}

impl Action {
    #[must_use]
    pub fn on_drive(&self) -> bool {
        matches!(
            self,
            Self::Trash { .. } | Self::Move { .. } | Self::Copy { .. }
        )
    }

    /// The tracked file whose entry depends on this action succeeding.
    #[must_use]
    pub fn tracked_id(&self) -> Option<&str> {
        match self {
            Self::Trash { id, .. }
            | Self::Move { id, .. }
            | Self::Write { id, .. }
            | Self::ConflictCopy { id, .. } => Some(id),
            Self::Remove { id, .. } | Self::KeepEdited { id, .. } => id.as_deref(),
            Self::Copy { .. } | Self::Warn { .. } => None,
        }
    }

    /// Drive first, then everything that frees a path, then the writes that
    /// may take one.
    fn stage(&self) -> u8 {
        match self {
            Self::Trash { .. } | Self::Move { .. } | Self::Copy { .. } => 0,
            Self::KeepEdited { .. } => 1,
            Self::ConflictCopy { .. } => 2,
            Self::Remove { .. } => 3,
            Self::Write { .. } => 4,
            Self::Warn { .. } => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub actions: Vec<Action>,
    /// The tracked links once every action succeeded.
    pub next: Tracked,
}

impl Plan {
    #[must_use]
    pub fn trash_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|action| matches!(action, Action::Trash { .. }))
            .count()
    }
}

#[must_use]
pub fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

fn base_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

fn stem(path: &str) -> &str {
    let name = base_name(path);
    split_link_name(name).map_or(name, |(stem, _)| stem)
}

fn ext_of(mime: &str) -> &'static str {
    kind_of_mime(mime).map_or("gdoc", |kind| kind.ext)
}

#[must_use]
pub fn render_remote(file: &RemoteFile) -> String {
    render(&file.id, &file.url, &file.name)
}

#[must_use]
pub fn render_tracked(id: &str, link: &Link) -> String {
    render(id, &link.url, &link.name)
}

fn edited(text: Option<&str>, ours: &[&str]) -> bool {
    text.is_none_or(|text| {
        let text = untitled(text);
        !ours.iter().any(|ours| untitled(ours) == text)
    })
}

type Group = (String, &'static str, String);

/// The link path of every remote file. A tracked file keeps its number;
/// new ones, by id, take none when it is free in their group, else the
/// lowest free number from 2. No two files share a path.
fn expected_paths<'a>(
    tracked: &Tracked,
    remote: &'a [RemoteFile],
) -> HashMap<&'a str, (String, Option<u32>)> {
    let mut sorted: Vec<&RemoteFile> = remote.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let group = |file: &RemoteFile| -> Group {
        (file.folder.clone(), ext_of(&file.mime), file.name.clone())
    };
    let path = |file: &RemoteFile, number| {
        join(
            &file.folder,
            &file_name(&file.name, number, ext_of(&file.mime)),
        )
    };
    let mut held: HashMap<Group, HashSet<Option<u32>>> = HashMap::new();
    let mut used: HashSet<String> = HashSet::new();
    let mut out = HashMap::new();
    let mut fresh = Vec::new();
    for file in sorted {
        let kept = tracked.links.get(&file.id).filter(|link| {
            link.folder == file.folder
                && link.name == file.name
                && ext_of(&link.mime) == ext_of(&file.mime)
        });
        let Some(link) = kept else {
            fresh.push(file);
            continue;
        };
        let target = path(file, link.number);
        let numbers = held.entry(group(file)).or_default();
        if numbers.contains(&link.number) || used.contains(&target) {
            fresh.push(file);
            continue;
        }
        numbers.insert(link.number);
        used.insert(target.clone());
        out.insert(file.id.as_str(), (target, link.number));
    }
    for file in fresh {
        let numbers = held.entry(group(file)).or_default();
        let mut number = None;
        loop {
            let target = path(file, number);
            if !numbers.contains(&number) && !used.contains(&target) {
                numbers.insert(number);
                used.insert(target.clone());
                out.insert(file.id.as_str(), (target, number));
                break;
            }
            number = Some(number.map_or(2, |n| n + 1));
        }
    }
    out
}

fn link_from(file: &RemoteFile, (path, number): &(String, Option<u32>)) -> Link {
    Link {
        path: path.clone(),
        name: file.name.clone(),
        folder: file.folder.clone(),
        mime: file.mime.clone(),
        url: file.url.clone(),
        number: *number,
        shortcut: file.shortcut,
    }
}

struct Builder {
    actions: Vec<Action>,
    next: Tracked,
}

impl Builder {
    fn push(&mut self, action: Action) {
        self.actions.push(action);
    }

    fn copy(&mut self, source: &str, link: &LocalLink, foreign: bool) {
        self.push(Action::Copy {
            source: source.to_owned(),
            name: drive_name(stem(&link.path)),
            folder: folder_of(&link.path).to_owned(),
            path: link.path.clone(),
            foreign,
        });
    }

    fn remove(&mut self, id: Option<&str>, link: &LocalLink, ours: &[&str], gone: bool) {
        if edited(link.text.as_deref(), ours) {
            self.push(Action::KeepEdited {
                id: id.map(str::to_owned),
                path: link.path.clone(),
            });
        }
        self.push(Action::Remove {
            id: id.map(str::to_owned),
            path: link.path.clone(),
            gone,
        });
    }

    /// Rewrites `link` with `text` when it differs, keeping a version of
    /// what is not one of `ours`.
    fn refresh(&mut self, id: &str, link: &LocalLink, text: &str, ours: &[&str]) {
        if link.text.as_deref() == Some(text) {
            return;
        }
        if edited(link.text.as_deref(), ours) {
            self.push(Action::KeepEdited {
                id: Some(id.to_owned()),
                path: link.path.clone(),
            });
        }
        self.push(Action::Write {
            id: id.to_owned(),
            path: link.path.clone(),
            text: text.to_owned(),
            kind: WriteKind::Refresh,
        });
    }

    fn write(&mut self, id: &str, path: &str, text: String, kind: WriteKind) {
        self.push(Action::Write {
            id: id.to_owned(),
            path: path.to_owned(),
            text,
            kind,
        });
    }

    #[allow(clippy::too_many_lines)]
    fn two_way(
        &mut self,
        id: &str,
        tracked: Option<&Link>,
        remote: Option<(&RemoteFile, &(String, Option<u32>))>,
        locals: &[&LocalLink],
    ) {
        let Some(link) = tracked else {
            self.untracked(id, remote, locals);
            return;
        };
        let before = render_tracked(id, link);
        let Some((file, expected)) = remote else {
            for local in locals {
                self.remove(Some(id), local, &[&before], true);
            }
            return;
        };
        let current = render_remote(file);
        let ours = [before.as_str(), current.as_str()];
        let target = &expected.0;
        let drive_moved = *target != link.path;
        let original = locals
            .iter()
            .position(|local| local.path == link.path)
            .or((!locals.is_empty()).then_some(0));
        for (index, local) in locals.iter().enumerate() {
            if Some(index) != original {
                self.copy(id, local, false);
            }
        }
        let Some(local) = original.map(|index| locals[index]) else {
            if drive_moved {
                self.write(id, target, current, WriteKind::New);
                self.next
                    .links
                    .insert(id.to_owned(), link_from(file, expected));
            } else {
                self.push(Action::Trash {
                    id: id.to_owned(),
                    path: link.path.clone(),
                });
            }
            return;
        };
        if local.path == link.path {
            if drive_moved {
                self.remove(Some(id), local, &ours, false);
                self.write(
                    id,
                    target,
                    current,
                    WriteKind::Renamed {
                        from: link.path.clone(),
                    },
                );
            } else {
                self.refresh(id, local, &current, &[&before]);
            }
            self.next
                .links
                .insert(id.to_owned(), link_from(file, expected));
        } else if local.path == *target {
            self.refresh(id, local, &current, &ours);
            self.next
                .links
                .insert(id.to_owned(), link_from(file, expected));
        } else if drive_moved {
            self.push(Action::ConflictCopy {
                id: id.to_owned(),
                from: local.path.clone(),
                to: target.clone(),
            });
            self.write(id, target, current, WriteKind::New);
            self.next
                .links
                .insert(id.to_owned(), link_from(file, expected));
        } else {
            let folder = folder_of(&local.path).to_owned();
            let (name, number) = if base_name(&local.path) == base_name(&link.path) {
                (file.name.clone(), link.number)
            } else {
                (drive_name(stem(&local.path)), None)
            };
            self.push(Action::Move {
                id: id.to_owned(),
                name: name.clone(),
                folder: folder.clone(),
                from: link.path.clone(),
                to: local.path.clone(),
            });
            self.next.links.insert(
                id.to_owned(),
                Link {
                    path: local.path.clone(),
                    name,
                    folder,
                    mime: file.mime.clone(),
                    url: file.url.clone(),
                    number,
                    shortcut: file.shortcut,
                },
            );
        }
    }

    /// A link whose id this sync does not track. When Drive shows the file
    /// in this sync, the link at its path is adopted and one elsewhere is
    /// set aside as a conflict; a link to a file outside is copied in.
    fn untracked(
        &mut self,
        id: &str,
        remote: Option<(&RemoteFile, &(String, Option<u32>))>,
        locals: &[&LocalLink],
    ) {
        let Some((file, expected)) = remote else {
            for local in locals {
                self.copy(id, local, true);
            }
            return;
        };
        let current = render_remote(file);
        let target = &expected.0;
        let original = locals.iter().position(|local| local.path == *target);
        let set_aside = original
            .is_none()
            .then_some(0)
            .filter(|_| !locals.is_empty());
        for (index, local) in locals.iter().enumerate() {
            if Some(index) != original && Some(index) != set_aside {
                self.copy(id, local, false);
            }
        }
        match (original, set_aside) {
            (Some(index), _) => self.refresh(id, locals[index], &current, &[]),
            (None, Some(index)) => {
                self.push(Action::ConflictCopy {
                    id: id.to_owned(),
                    from: locals[index].path.clone(),
                    to: target.clone(),
                });
                self.write(id, target, current, WriteKind::New);
            }
            (None, None) => self.write(id, target, current, WriteKind::New),
        }
        self.next
            .links
            .insert(id.to_owned(), link_from(file, expected));
    }

    /// Drive is the only truth: every link not where Drive puts it goes.
    fn one_way(
        &mut self,
        id: &str,
        tracked: Option<&Link>,
        remote: Option<(&RemoteFile, &(String, Option<u32>))>,
        locals: &[&LocalLink],
    ) {
        let before = tracked.map(|link| render_tracked(id, link));
        let Some((file, expected)) = remote else {
            let ours: Vec<&str> = before.iter().map(String::as_str).collect();
            for local in locals {
                self.remove(Some(id), local, &ours, tracked.is_some());
            }
            return;
        };
        let current = render_remote(file);
        let mut ours = vec![current.as_str()];
        ours.extend(before.as_deref());
        let target = &expected.0;
        let mut at_target = false;
        let mut removed_tracked = None;
        for local in locals {
            if local.path == *target {
                at_target = true;
                self.refresh(id, local, &current, &ours);
            } else {
                if tracked.is_some_and(|link| link.path == local.path) {
                    removed_tracked = Some(local.path.clone());
                }
                self.remove(Some(id), local, &ours, false);
            }
        }
        if !at_target {
            let kind = removed_tracked.map_or(WriteKind::New, |from| WriteKind::Renamed { from });
            self.write(id, target, current, kind);
        }
        self.next
            .links
            .insert(id.to_owned(), link_from(file, expected));
    }
}

/// `local` must be sorted by path, as [`super::link::scan`] returns it.
#[must_use]
pub fn plan(tracked: &Tracked, local: &[LocalLink], remote: &[RemoteFile], two_way: bool) -> Plan {
    let expected = expected_paths(tracked, remote);
    let remote_by_id: HashMap<&str, &RemoteFile> =
        remote.iter().map(|file| (file.id.as_str(), file)).collect();
    let mut by_id: BTreeMap<&str, Vec<&LocalLink>> = BTreeMap::new();
    let mut without_id = Vec::new();
    for link in local {
        match link.id.as_deref() {
            Some(id) => by_id.entry(id).or_default().push(link),
            None => without_id.push(link),
        }
    }
    let ids: BTreeSet<&str> = tracked
        .links
        .keys()
        .map(String::as_str)
        .chain(remote_by_id.keys().copied())
        .chain(by_id.keys().copied())
        .collect();
    let mut builder = Builder {
        actions: Vec::new(),
        next: Tracked::default(),
    };
    for id in ids {
        let locals = by_id.get(id).map_or(&[][..], Vec::as_slice);
        let link = tracked.links.get(id);
        let remote = remote_by_id
            .get(id)
            .and_then(|file| expected.get(id).map(|expected| (*file, expected)));
        if two_way {
            builder.two_way(id, link, remote, locals);
        } else {
            builder.one_way(id, link, remote, locals);
        }
    }
    for link in without_id {
        if two_way {
            builder.push(Action::Warn {
                path: link.path.clone(),
            });
        } else {
            builder.remove(None, link, &[], false);
        }
    }
    builder.actions.sort_by_key(Action::stage);
    Plan {
        actions: builder.actions,
        next: builder.next,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gdocs::link::parse_id;

    const DOC: &str = "application/vnd.google-apps.document";

    fn doc(id: &str, folder: &str, name: &str) -> RemoteFile {
        RemoteFile {
            id: id.into(),
            name: name.into(),
            mime: DOC.into(),
            folder: folder.into(),
            parent: format!("folder-{folder}"),
            url: format!("https://docs.google.com/document/d/{id}/edit"),
            shortcut: false,
        }
    }

    fn local_from(path: &str, text: &str) -> LocalLink {
        LocalLink {
            path: path.into(),
            id: parse_id(text).map(str::to_owned),
            text: Some(text.into()),
        }
    }

    /// The state after a round that wrote every link of `remote`.
    fn settled(remote: &[RemoteFile]) -> (Tracked, Vec<LocalLink>) {
        let first = plan(&Tracked::default(), &[], remote, true);
        let mut disk: Vec<LocalLink> = first
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::Write { path, text, .. } => Some(local_from(path, text)),
                _ => None,
            })
            .collect();
        disk.sort_by(|a, b| a.path.cmp(&b.path));
        (first.next, disk)
    }

    fn moved(disk: &[LocalLink], from: &str, to: &str) -> Vec<LocalLink> {
        let mut out: Vec<LocalLink> = disk
            .iter()
            .map(|link| {
                let mut link = link.clone();
                if link.path == from {
                    link.path = to.into();
                }
                link
            })
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    fn drive_actions(plan: &Plan) -> Vec<&Action> {
        plan.actions
            .iter()
            .filter(|action| action.on_drive())
            .collect()
    }

    #[test]
    fn nothing_changed_plans_nothing() {
        let remote = [doc("d1", "a", "Ata")];
        let (tracked, disk) = settled(&remote);

        let plan = plan(&tracked, &disk, &remote, true);

        assert!(plan.actions.is_empty());
        assert_eq!(plan.next, tracked);
    }

    #[test]
    fn a_new_file_on_drive_gets_a_link_at_its_path() {
        let remote = [doc("d1", "a", "Ata")];

        let plan = plan(&Tracked::default(), &[], &remote, true);

        assert!(matches!(
            plan.actions.as_slice(),
            [Action::Write { id, path, kind: WriteKind::New, .. }]
                if id == "d1" && path == "a/Ata.gdoc.html"
        ));
    }

    #[test]
    fn a_deleted_link_sends_the_file_to_the_trash() {
        let remote = [doc("d1", "a", "Ata")];
        let (tracked, _) = settled(&remote);

        let plan = plan(&tracked, &[], &remote, true);

        assert_eq!(
            plan.actions,
            [Action::Trash {
                id: "d1".into(),
                path: "a/Ata.gdoc.html".into()
            }]
        );
        assert!(plan.next.links.is_empty());
    }

    #[test]
    fn a_folder_deleted_on_disk_trashes_every_doc_in_it() {
        let remote = [doc("d1", "a", "Ata"), doc("d2", "a", "Notes")];
        let (tracked, _) = settled(&remote);

        let plan = plan(&tracked, &[], &remote, true);

        assert_eq!(plan.trash_count(), 2);
    }

    #[test]
    fn a_renamed_link_renames_the_same_file_on_drive() {
        let remote = [doc("d1", "a", "Ata")];
        let (tracked, disk) = settled(&remote);
        let disk = moved(&disk, "a/Ata.gdoc.html", "a/Minutes.gdoc.html");

        let plan = plan(&tracked, &disk, &remote, true);

        assert_eq!(
            plan.actions,
            [Action::Move {
                id: "d1".into(),
                name: "Minutes".into(),
                folder: "a".into(),
                from: "a/Ata.gdoc.html".into(),
                to: "a/Minutes.gdoc.html".into(),
            }]
        );
        assert_eq!(plan.next.links["d1"].path, "a/Minutes.gdoc.html");
    }

    #[test]
    fn a_link_renamed_on_disk_is_retitled_without_keeping_a_version() {
        let remote = [doc("d1", "a", "Ata")];
        let (tracked, disk) = settled(&remote);
        let disk = moved(&disk, "a/Ata.gdoc.html", "a/Minutes.gdoc.html");
        let renamed = plan(&tracked, &disk, &remote, true).next;

        let after = plan(&renamed, &disk, &[doc("d1", "a", "Minutes")], true);

        assert!(matches!(
            after.actions.as_slice(),
            [Action::Write { path, kind: WriteKind::Refresh, .. }] if path == "a/Minutes.gdoc.html"
        ));
    }

    #[test]
    fn a_link_moved_to_another_folder_moves_the_file_and_keeps_its_name() {
        let remote = [doc("d1", "a", "Ata"), doc("d2", "a", "Ata")];
        let (tracked, disk) = settled(&remote);
        let disk = moved(&disk, "a/Ata (2).gdoc.html", "b/Ata (2).gdoc.html");

        let plan = plan(&tracked, &disk, &remote, true);

        assert_eq!(
            plan.actions,
            [Action::Move {
                id: "d2".into(),
                name: "Ata".into(),
                folder: "b".into(),
                from: "a/Ata (2).gdoc.html".into(),
                to: "b/Ata (2).gdoc.html".into(),
            }]
        );
        assert_eq!(plan.next.links["d2"].number, Some(2));
    }

    #[test]
    fn a_copied_link_copies_the_file_and_leaves_the_original_alone() {
        let remote = [doc("d1", "a", "Ata")];
        let (tracked, mut disk) = settled(&remote);
        disk.push(LocalLink {
            path: "b/Ata copy.gdoc.html".into(),
            ..disk[0].clone()
        });

        let plan = plan(&tracked, &disk, &remote, true);

        assert_eq!(
            plan.actions,
            [Action::Copy {
                source: "d1".into(),
                name: "Ata copy".into(),
                folder: "b".into(),
                path: "b/Ata copy.gdoc.html".into(),
                foreign: false,
            }]
        );
    }

    #[test]
    fn renamed_on_both_sides_drive_wins_and_the_local_name_becomes_a_conflict() {
        let (tracked, disk) = settled(&[doc("d1", "a", "Ata")]);
        let disk = moved(&disk, "a/Ata.gdoc.html", "a/Mine.gdoc.html");
        let remote = [doc("d1", "a", "Theirs")];

        let plan = plan(&tracked, &disk, &remote, true);

        assert!(drive_actions(&plan).is_empty());
        assert!(matches!(
            plan.actions.as_slice(),
            [
                Action::ConflictCopy { from, to, .. },
                Action::Write { path, .. },
            ] if from == "a/Mine.gdoc.html" && to == "a/Theirs.gdoc.html"
                && path == "a/Theirs.gdoc.html"
        ));
    }

    #[test]
    fn a_rename_on_drive_moves_the_link_on_disk() {
        let (tracked, disk) = settled(&[doc("d1", "a", "Ata")]);
        let remote = [doc("d1", "a", "Minutes")];

        let plan = plan(&tracked, &disk, &remote, true);

        assert!(matches!(
            plan.actions.as_slice(),
            [
                Action::Remove { path: removed, gone: false, .. },
                Action::Write { path, kind: WriteKind::Renamed { from }, .. },
            ] if removed == "a/Ata.gdoc.html" && path == "a/Minutes.gdoc.html"
                && from == "a/Ata.gdoc.html"
        ));
    }

    #[test]
    fn a_file_deleted_on_drive_takes_its_link_away() {
        let (tracked, disk) = settled(&[doc("d1", "a", "Ata")]);

        let plan = plan(&tracked, &disk, &[], true);

        assert_eq!(
            plan.actions,
            [Action::Remove {
                id: Some("d1".into()),
                path: "a/Ata.gdoc.html".into(),
                gone: true
            }]
        );
    }

    #[test]
    fn an_edited_link_is_kept_as_a_version_and_written_again() {
        let remote = [doc("d1", "a", "Ata")];
        let (tracked, mut disk) = settled(&remote);
        disk[0].text = Some(format!("{}<!-- mine -->", disk[0].text.clone().unwrap()));

        let plan = plan(&tracked, &disk, &remote, true);

        assert!(matches!(
            plan.actions.as_slice(),
            [
                Action::KeepEdited { path: kept, .. },
                Action::Write { path, kind: WriteKind::Refresh, .. },
            ] if kept == "a/Ata.gdoc.html" && path == "a/Ata.gdoc.html"
        ));
    }

    #[test]
    fn duplicate_names_are_numbered_and_a_number_is_never_reassigned() {
        let remote = [doc("d1", "", "Ata"), doc("d2", "", "Ata")];
        let (tracked, disk) = settled(&remote);
        let paths: Vec<&str> = disk.iter().map(|link| link.path.as_str()).collect();
        assert_eq!(paths, ["Ata (2).gdoc.html", "Ata.gdoc.html"]);

        let after = plan(&tracked, &disk[..1], &[doc("d2", "", "Ata")], true);

        assert!(after.actions.is_empty());
        assert_eq!(after.next.links["d2"].path, "Ata (2).gdoc.html");
    }

    #[test]
    fn a_shortcut_has_its_own_link_and_only_the_shortcut_goes_to_the_trash() {
        let target = doc("t1", "", "Plan");
        let shortcut = RemoteFile {
            id: "s1".into(),
            url: "https://drive.google.com/open?id=t1".into(),
            shortcut: true,
            ..doc("s1", "a", "Plan")
        };
        let remote = [target, shortcut];
        let (tracked, disk) = settled(&remote);
        let without_shortcut: Vec<LocalLink> = disk
            .into_iter()
            .filter(|link| link.path != "a/Plan.gdoc.html")
            .collect();

        let plan = plan(&tracked, &without_shortcut, &remote, true);

        assert_eq!(
            plan.actions,
            [Action::Trash {
                id: "s1".into(),
                path: "a/Plan.gdoc.html".into()
            }]
        );
    }

    #[test]
    fn a_link_to_a_file_outside_the_sync_is_copied_in() {
        let text = render("x9", "https://docs.google.com/document/d/x9/edit", "Other");

        let plan = plan(
            &Tracked::default(),
            &[local_from("Other.gdoc.html", &text)],
            &[],
            true,
        );

        assert!(matches!(
            plan.actions.as_slice(),
            [Action::Copy { source, foreign: true, .. }] if source == "x9"
        ));
    }

    #[test]
    fn a_link_without_an_id_stays_and_warns() {
        let plan = plan(
            &Tracked::default(),
            &[local_from("Hand.gdoc.html", "hello")],
            &[],
            true,
        );

        assert_eq!(
            plan.actions,
            [Action::Warn {
                path: "Hand.gdoc.html".into()
            }]
        );
    }

    #[test]
    fn a_lost_state_adopts_the_links_where_drive_puts_them() {
        let remote = [doc("d1", "a", "Ata")];
        let (tracked, disk) = settled(&remote);

        let plan = plan(&Tracked::default(), &disk, &remote, true);

        assert!(plan.actions.is_empty());
        assert_eq!(plan.next, tracked);
    }

    #[test]
    fn one_way_never_touches_drive_and_puts_the_drive_state_back() {
        let remote = [doc("d1", "a", "Ata"), doc("d2", "a", "Notes")];
        let (tracked, disk) = settled(&remote);
        let mut disk = moved(&disk, "a/Ata.gdoc.html", "b/Ata.gdoc.html");
        disk.push(local_from(
            "Foreign.gdoc.html",
            &render("x9", "https://x", "F"),
        ));
        let disk: Vec<LocalLink> = disk
            .into_iter()
            .filter(|link| link.path != "a/Notes.gdoc.html")
            .collect();

        let plan = plan(&tracked, &disk, &remote, false);

        assert!(drive_actions(&plan).is_empty());
        let mut writes: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::Write { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        writes.sort_unstable();
        assert_eq!(writes, ["a/Ata.gdoc.html", "a/Notes.gdoc.html"]);
        let mut removed: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|action| match action {
                Action::Remove { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        removed.sort_unstable();
        assert_eq!(removed, ["Foreign.gdoc.html", "b/Ata.gdoc.html"]);
    }
}
