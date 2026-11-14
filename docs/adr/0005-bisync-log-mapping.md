# 0005: Mapping the bisync log to changes

## Context
The review sheet, Activity and the notifications need per-file changes and counters. rclone logs them as object events with no side, in a text format that differs a little between real and `--dry-run` runs. The mapping is derived from the fixtures in `crates/syncfast-core/tests/fixtures/`.

## Decision
- The side is the target of the last `- PathN  Do queued copies to  - PathM` or `Resync is copying files to  - PathM` line. Copies, replacements and deletions that follow happen on PathM (Path1 is local, Path2 is remote).
- Real and dry-run events map to the same kinds: `Copied (new)` or `Skipped copy` is an upload or download; `Deleted` or `Skipped delete` is a deletion; `Moved into backup dir` or `Skipped move into backup dir` is a local deletion.
- A copy is an overwrite when rclone says `Copied (replaced existing)`, when the old file moved into `--backup-dir1` just before (`Moved (server-side)`, `Skipped move`), or when the source side lists `File changed` and the target side does not list `File was deleted`. A file deleted on one side and changed on the other is copied back as a plain upload or download.
- Counters are `None` unless the log ends with `Bisync successful`; an interrupted run has only a partial picture.
- `Other` holds dry-run skips with no mapping, folder creation and removal (shown with a trailing `/`), and notice or warning events about filesystem objects. Errors are never changes. Folder timestamp updates are ignored.
- Repeated events for the same kind and path are listed once.
- A tracked rename is one change, `rename_local` or `rename_remote` by the side it happens on, with the old path in `from`. It comes from the `Renamed from "<old>"` notice rclone logs on the new path; the `Moved (server-side) to:` line on the old path and, in a dry run, the folder creation and removal around the renamed files are dropped. A one-way mirror reports the rename as `+ new` and `- old` in its `--combined` report; the notice pairs them back. `renamed` counts renames, which are never deletions.
- A resync review lists in `returning` the downloads of files the last good local listing had and the uploads of files the last good remote listing had: files deleted or renamed since then that the resync brings back. The listings are the newest `.lst` pair in the bisync workdir, or its `.lst-err`.
- A normal round that ends `Ok` with no change, no new conflict and no automatic review keeps its log as `<stamp>.idle.jsonl`. Only the newest idle log is kept, so idle checks do not push real rounds out of the 50 kept in the history.
- `state.json` keeps only the first 100 changes of the last run (counters stay complete): the GUI rereads the file every second or two. The full list stays in the run's log.
- The JSON these changes reach (`review show`, `preview`, `activity`, `show`) only grows: new kinds and fields, never a renamed or removed one.

## Consequences
- A dry-run `--resync` cannot tell a new file from a replacement on the remote, because nothing lists it as changed: it shows as an upload.
- An rclone upgrade can change message text; the fixtures and `just capture-fixtures` catch that.
- `--backup-dir1` does act during `--resync` (checked with the real rclone), so the review keeps the "old version kept" wording for local replacements.
