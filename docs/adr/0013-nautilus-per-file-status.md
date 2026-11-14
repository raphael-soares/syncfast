# 0013: The sync state of every file and folder in Nautilus

## Context
ADR 0012 put an emblem on each sync's root and on conflicted files. The wish now is the OneDrive behaviour: every file and folder inside a sync shows whether it is synced, waiting for the next round, syncing, failed or in conflict, and the list view has a column with the same state as text ("Synced 5 min ago", "Waiting", "Failed: permission denied"). syncfast keeps no per-file database: rclone bisync and the one-way modes hold the listings, and syncfast only stores the summary of the last round.

## Decision
- The service gains `Directory(s) -> a(sss)`: the name, state and detail of each direct child of a folder inside a sync. States: `synced` (detail: when the last good round finished, RFC 3339), `pending`, `syncing`, `error` (detail: rclone's message) and `conflict`. A folder outside every sync, or one the filters leave out, answers an empty list. Entries the filters leave out are not listed. `Roots`, `Conflicts`, `SyncNow` and `Changed` stay as they were.
- An entry's state comes from three sources, in this order: the conflicted files the service already scans (`conflict`); the files the last round could not sync, which the round summary now keeps (`RunSummary.errors`, from the error lines of rclone's log that name an object and, for the one-way modes, the `!` lines of the report); and whether it changed since the last good round (`pending`, or `syncing` while the round runs). Otherwise `synced`. A folder takes `error` or `pending` from anything below it.
- "Changed since the last good round" is two checks. The entry's ctime is later than the round's end: this covers changes made while the watcher was not running and syncs with `watch` off. Or the watcher saw the path change after the round started: a change made during the round has a ctime before its end, and a file the round downloaded must not look changed, which the ctime alone cannot tell apart. The watcher keeps those paths in memory per sync (at most 10 000; past that, the whole sync counts as changed) and the service forgets the ones older than the start of the last good round.
- The last good round is `last_run` when it is the one that set `last_success`, else a round that started and ended at `last_success`. A sync that never succeeded is all `pending`.
- `Directory` is the one call that does I/O: it lists the folder and reads each entry's metadata, off the async runtime. The service never walks the tree for it.
- File rules of the filters (`- *.bak`, `- /top.txt`) now count for this listing through `DirMatcher::is_excluded_file`. `DirMatcher::is_excluded`, used by the watcher, the conflict scan and the backup, still reads only folder rules.
- The extension asks `Directory` for the folder of a file Nautilus shows and caches the answer (64 folders, least recently used goes). On `Changed` it asks again for every cached folder and invalidates only the files whose answer changed. Emblems come from the icon theme: `emblem-default` (synced), `emblem-synchronizing` (pending, syncing), `emblem-important` (error, conflict). The column is a `Nautilus.ColumnProvider` with the attribute `syncfast_status`, labelled "Sync".

## Consequences
- The "Synced N min ago" text is computed when Nautilus asks for the file; it only moves when the folder is refreshed or a round ends.
- A file the round downloaded makes the watcher see a change during the round, so it shows as pending until the round that follows it (the watcher already starts that round).
- A change of the filters alone does not emit `Changed`; the review it triggers does.
- `Conflicts` is no longer used by the extension but stays on the bus.
