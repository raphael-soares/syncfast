# 0010: Google Docs as link files kept by Drive ID

## Context
Native Google files (Docs, Sheets, Slides, Forms, Drawings, My Maps, Sites) have no bytes to download, so rclone either skips them (`--drive-skip-gdocs`), exports them to Office files, or writes link files. Users still need to see and open them from the synced folder, and deleting a folder on disk must not leave its Docs behind on Drive, where they bring the folder back on the next round (issue #125).

Two spikes against a real Drive settled what rclone can do. The first (rclone 1.75.2, `docs/design/gdocs-spike.md`) rejected `export`: a change on both sides sends the Doc to the trash and creates another one with a new ID, losing its sharing and comments. The second (rclone 1.76.0, `docs/design/gdocs-links-spike.md`, fixtures in `crates/syncfast-core/tests/fixtures/rclone-1.76.0/gdocs/`) found that rclone's link formats (`desktop`, `url`, `link.html`) all behave the same: bisync knows a file only by its path, so renaming or moving a link deletes the Doc and uploads a plain file. A mounted disk was ruled out because it only works online.

## Decision
- rclone never sees a Google file. Every round passes `--drive-skip-gdocs` and the filter `- *.{gdoc,gsheet,gslides,gform,gdraw,gmap,gsite}.html*` ahead of the user's filters, so link files and their conflict copies never reach Drive and no user rule can admit them.
- syncfast writes one link file per Google file, `<name>.<ext>.html` (`Ata.gdoc.html`, `Budget.gsheet.html`). It is a small HTML page that redirects to the file and carries its Drive ID in `<meta name="syncfast-drive-id">`. A `/` in the Drive name becomes `／`, and files of one folder with the same name and kind get ` (2)`, ` (3)`, a number that is never reassigned while the file lives. A shortcut gets its own link, which opens the target.
- The setting `google_docs` (default on) turns this on for `bisync`, `download` and `mirror_down`. Turning it off, or switching to `upload` or `mirror_up`, deletes every link that is still as syncfast wrote it and forgets them.
- `gdocs.json` in the sync's state folder records, by Drive ID, the path, name, folder and URL of each link last written. A pure planner (`gdocs::plan`) compares it with the links on disk (found by ID, not by path) and the files on Drive (`files.list` per folder of the sync), and decides:
  - In `bisync`, a link deleted on disk sends the file to the Drive trash; renamed or moved on disk, the same file is renamed or moved on Drive; copied on disk, the file is copied on Drive and the copy gets the new ID. A file new, renamed, moved or deleted on Drive gets its link written, moved or removed on disk. A link edited by hand is kept in the versions folder and written again. When both sides changed, Drive wins and the local link becomes a `.conflict<N>` file. A link to a file outside the sync is copied in; one without a usable ID stays and is reported.
  - In `download` and `mirror_down`, Drive is the only truth: links are put back where Drive has them, read-only, and nothing is changed on Drive.
- The Drive part runs before rclone, so a Doc deleted with its folder is in the trash before bisync removes the folder. The disk part runs after a successful round. A normal round that would trash more than `max_delete` percent of the tracked Docs stops at the same review as an rclone cap, and the review lists the Doc changes.
- Restoring a trashed Google file from Recently Deleted takes it out of the Drive trash into the folder at its old path, made again when missing (a trashed folder stays trashed); the round that follows writes its link.

## Consequences
- Rename, move, share and comment history of a Doc survive any change made through its link, because the file keeps its ID.
- The Drive token needs the full `drive` scope (rclone's default) for the trash, rename, move and copy calls. Without it the Drive part stops at the first refusal and logs it once per round; links are still written.
- A failed Drive listing skips the Docs for that round; rclone still runs.
- A `gdocs.json` that is not valid JSON is set aside as `gdocs.json.corrupt-<stamp>`; the next round adopts the links where Drive has them and changes nothing on Drive for the lost entries.
- An rclone upgrade may change link or filter handling. Rerun `tests/capture_gdocs.sh` against a test folder before relying on it.
