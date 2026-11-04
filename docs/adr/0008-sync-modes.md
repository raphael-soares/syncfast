# 0008: Sync modes

## Context
Phase 1 had one way to sync, `rclone bisync`. Some folders only need to go one way: a camera folder that is uploaded, a Drive folder that is read on this computer, a backup-like copy that must match its source. Bisync cannot do that safely, and `rclone sync` does not refuse an empty source: it deletes the destination.

## Decision
- `mode` is a setting like the others (`[defaults]` or per sync), default `bisync`. Local stays Path1 and the source or destination follows the mode:
  - `upload`: `rclone copy` local to remote. Never deletes.
  - `download`: `rclone copy` remote to local. Never deletes; replaced local files go to versions.
  - `mirror_up`: `rclone sync` local to remote. The remote becomes identical to the local folder.
  - `mirror_down`: `rclone sync` remote to local. The local folder becomes identical to the remote.
- The one-way modes run behind the same gates as bisync (ADR 0002): first run, changed filters, a changed mode and a deletion count over `max_delete` (`too_many_deletes`). The review is a `--dry-run` of the same engine and the `review` level decides who confirms it. There is no critical-error or all-files-changed gate: there are no listings.
- A mirror never empties its destination. Before every one-way run, with any `review` level and also for a confirmed review, syncfast checks that the source holds at least one file (`rclone lsf -R --files-only` with the filters, stopped at the first name). An empty source is `needs_attention` (`Fault::LocalEmpty` or `Fault::RemoteEmpty`) and nothing runs.
- The cap is `--max-delete <N>` with `N = floor(dest_files * max_delete / 100)`. In `rclone sync` that flag counts files (bisync's is a percentage), so `dest_files` is kept in `state.json` after each good mirror run. A confirmed review runs without the cap.
- What the local side loses goes to `$XDG_DATA_HOME/syncfast/versions/<id>/<stamp>/` through `--backup-dir` in every mode that can lose something there. What the remote side loses goes to `<account>:.syncfast-versions/<id>/<stamp>` only in `mirror_up`, where deletions are the point; `bisync` and `upload` rely on the Drive trash and revisions as before. rclone needs the backup dir on the same remote and outside the destination, so `mirror_up` with versions needs a folder of the account, not its root, and the versions folder counts as occupied remote space for the overlap rule. Old remote folders are purged by the date in their name.
- Changing the mode of a synced pair is a gate (`needs_review_mode_changed`) that follows the `review` level. Leaving bisync deletes the bisync listings after the first good round of the new mode; coming back needs a `--resync` review, which only copies, so it deletes nothing.
- Changes are read from rclone's `--combined` report (`+` new, `*` differs, `-` only on the destination) instead of the bisync log, and runs use `--retries 1` so the report has no repeats.
- Watching the local folder applies only where the local folder is the source (`bisync`, `upload`, `mirror_up`); conflict settings apply to `bisync` only. Settings that do not apply to a mode are accepted and ignored.
- Agents cannot switch a sync to a mirror mode, or flip a mirror's direction (ADR 0002, ADR 0007).

## Consequences
- `rclone sync` skips every deletion when a read error happens during the run (rclone docs), so a flaky source cannot delete half the destination.
- The one-way fixtures (`oneway_*`) pin `--combined` under `--dry-run`, the `--max-delete` exit code and text, and the early exit of `lsf`; an rclone upgrade that changes them fails the tests.
- Syncs on the same account that sit at its root overlap a `mirror_up` sync's versions folder, so the config rejects them (`remote folder overlaps "<name>"`).
- `upload` replaces a newer remote file with an older local one, as `rclone copy` does. Drive keeps the replaced revision.
