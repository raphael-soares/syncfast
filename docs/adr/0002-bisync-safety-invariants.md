# 0002: Bisync safety invariants

## Context
`rclone bisync` can delete or overwrite data on both sides. A wrong flag or an unreviewed resync can destroy a Drive folder or a local tree.

## Decision
- The bisync arguments are fixed and built in one place (`bisync_args`), driven by a `Behavior` struct whose defaults are the phase 1 values. Local is always Path1, remote Path2.
- Review is mandatory: the first run, a changed filter set, a non-recoverable critical error and `--max-delete` overruns gate the pair. A gated pair runs again only after the user sees a dry-run preview and confirms. `--resync` and `--force` are never used without that confirmation marker.
- `--exclude-if-present` is never used: `git init` in an already synced folder would make bisync delete it on Drive. `.git/` is excluded by a filter preset instead.
- Anything deleted or overwritten on the local side goes to `$XDG_DATA_HOME/syncfast/versions/<id>/` through `--backup-dir1`, in one dated folder per run (`<AAAAMMDD-HHMMSS>/`), and is pruned after 30 days. No `--suffix` is used: it would rename the old remote copy beside the file and sync it back as a new file. The remote side relies on the Drive trash and revisions.
- Google Docs are skipped (`--drive-skip-gdocs`).

## Consequences
- Some rounds stop and wait for the user; the UI and tray say so.
- The preview comes from `--dry-run`, which may list deletions the real run would not do. The UI says that.
- Phase 2 can expose each `Behavior` field without touching the engine.
