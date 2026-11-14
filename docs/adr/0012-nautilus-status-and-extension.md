# 0012: Sync status on the session bus and a Nautilus extension

## Context
The wish: see in Nautilus which folder is a sync, whether it is up to date, and which files are conflicted, and start a round from the context menu. Nautilus runs on the host, outside the sandbox, and cannot read syncfast's state. The app is only distributed as a Flatpak (ADR 0006), so the host side has to be a separate, optional piece.

## Decision
- `syncfast watch` publishes `io.github.raphael_soares.syncfast.Status` on the session bus (object `/io/github/raphael_soares/syncfast/Status`, interface of the same name), from `status_bus.rs`. `Roots() -> a(ss)` is the absolute local folder and state of each enabled sync, `Conflicts() -> as` the absolute paths of conflicted files, `SyncNow(s) -> b` starts the round of the sync whose folder holds the path (the watcher's own trigger), and `Changed()` is emitted when either answer changes.
- The state of a root is `root_state` in this order: `attention` (the sync has a gate), `conflict` (`conflicts_pending > 0`), `syncing` (the pair's lock is held), `pending` (the watcher has a trigger on its way: events debouncing, waiting for a round or settling after it), else `synced`.
- The service reads, every 2 seconds and when the config or the watcher's waiting set changes, each sync's `state.json` and lock file, and answers from that snapshot, so a call never does I/O. `Changed` follows a difference between two snapshots. `state.json` only holds the number of conflicts, not the files, so the tree is walked (`find_conflicts`, filters respected) only for a sync with conflicts, and again only when a round ended or the count changed. `Conflicts` lists each conflict file and its original when it still exists.
- A root matches a path by whole components (`/a/Drive2` is not inside `/a/Drive`); nested roots answer with the innermost.
- A name under the app id prefix needs no permission in the Flatpak, so the manifest is unchanged. Without a session bus the watcher logs a warning and goes on, as it does without the system bus.
- The extension `packaging/nautilus/syncfast_nautilus.py` (python-nautilus 4, Nautilus 43 and later) caches both answers and refreshes them on `Changed` or when the name appears. It never calls D-Bus inside `update_file_info`: it reads the cache, and when an emblem changes it looks the file up among the ones Nautilus has loaded (`Nautilus.FileInfo.lookup`) and calls `invalidate_extension_info` on it, so the extension holds no file of its own. Emblems: `emblem-default` on a synced root, `emblem-synchronizing` on a pending or syncing one, `emblem-important` on a root in `conflict` or `attention` and on every conflicted file. "Sync Now" shows on files and folders inside a root. Without the service there is no emblem and no menu. The decisions (which emblem, which root) are plain functions, checked with `python3 -m unittest` in `packaging/nautilus/`.
- The extension is the AUR package `syncfast-nautilus` (`packaging/aur/syncfast-nautilus/`), which depends on `python-nautilus` and `python-gobject`. Fedora and other file managers are not covered; the app is complete without it.
- The state of every file and folder inside a sync, and the "Sync" column: ADR 0013.

## Consequences
- A state change reaches Nautilus within about 2 seconds, and a round started from another process shows as `syncing` only while its lock is held.
- A sync that is not watched (`watch` off) still has a root and a state; `pending` only exists for syncs the watcher follows.
- The service exists only while `syncfast watch` runs.
- The extension loaded in Nautilus is not covered by tests (no Nautilus in CI); the service over a private bus, the state order, the root lookup, the change detection and the extension's decisions are.
- The package version is not bumped by release-please; the extension changes rarely and the owner bumps it when it does.
