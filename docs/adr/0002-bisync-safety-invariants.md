# 0002: Bisync safety invariants

## Context
`rclone bisync` can delete or overwrite data on both sides. A wrong flag or an unreviewed resync can destroy a Drive folder or a local tree.

## Decision
- The bisync arguments are fixed and built in one place (`bisync_args`), driven by a `Behavior` struct whose defaults are the phase 1 values. Local is always Path1, remote Path2.
- Review is mandatory: the first run, a changed filter set, a non-recoverable critical error, `--max-delete` overruns and rclone's "all files were changed" safety abort gate the pair. A gated pair runs again only after the user sees a dry-run preview and confirms. `--resync` and `--force` are never used without that confirmation marker.
- `--exclude-if-present` is never used: `git init` in an already synced folder would make bisync delete it on Drive. `.git/` is excluded by a filter preset instead.
- Anything deleted or overwritten on the local side goes to `$XDG_DATA_HOME/syncfast/versions/<id>/` through `--backup-dir1`, in one dated folder per run (`<AAAAMMDD-HHMMSS>/`), and is pruned after 30 days. No `--suffix` is used: it would rename the old remote copy beside the file and sync it back as a new file. The remote side relies on the Drive trash and revisions.
- A confirmed marker survives a `Transient` failure and stays valid while its `filters_hash` matches; any other outcome that sets a gate deletes it.
- A pending gate is never replaced by `local_missing`: the review survives the folder going away and coming back. `config_error` ends by itself once the pair is valid again (a pair that never synced goes back to its first review).
- The "all files were changed" abort (every file of a side changed at once, typical of a one-file folder) is cleared like a mass deletion: a `--force` round after a confirmed preview.
- A normal round that fails with `Empty prior Path1 listing` or `Empty prior Path2 listing` (the pair was empty on both sides at its last sync, so rclone keeps empty listings and refuses to continue) is retried once in the same `run` as a `--resync`. It needs no review: a resync only copies, and an empty prior listing leaves no known deletion to propagate.
- Google Docs are skipped (`--drive-skip-gdocs`).

## Consequences
- Some rounds stop and wait for the user; the UI and tray say so.
- The preview comes from `--dry-run`, which may list deletions the real run would not do. The UI says that.
- Phase 2 can expose each `Behavior` field without touching the engine.
