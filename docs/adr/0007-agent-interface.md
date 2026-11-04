# 0007: Interface for AI agents

## Context
The owner wants AI agents to do everything a user does in syncfast, except sign in, and to explore Drive and local folders to help decide what to sync. The GUI has its logic in its backend, where a CLI or a server cannot reach it.

## Decision
- `syncfast-core::ops` holds one function per operation (`Ops::status`, `add`, `show_review`, `confirm_review`, `explore`...) that returns `Serialize` structs. The CLI (`--json` on every subcommand) and `syncfast mcp` only translate arguments and print. The MCP server uses the official `rmcp` SDK (it passes `cargo deny`).
- Authentication stays with the user: no operation creates an account, changes credentials or runs OAuth. `accounts` lists names, types and the shared client ID warning, never a secret.
- Exploring reads metadata only (`rclone lsjson -R`): names, sizes, dates, counts per folder and extension. Output is capped by depth and by a maximum number of folders.
- A held sync is released by a review. `review show` writes what it showed to `pairs/<id>/preview.json`. `review confirm` accepts it only when the given `filters_hash` is the shown one, the filters have not changed since, the gate still waits for that kind of review, and the review has no `DeleteLocal` or `DeleteRemote`. Otherwise nothing changes: exit 4 with `This review deletes files. Confirm it in syncfast.` for deletions, exit 5 for a review that was not shown or is stale. There is no flag around it.
- Lowering safety is user-only, like confirming a deletion. A setting change from the CLI or MCP that makes data loss easier (a `review` level above `manual`, a higher `max_delete`, `versions = false`, `conflict_loser = delete`, a switch to a mirror mode) is refused with exit 4 and writes nothing; the GUI writes it. The backup key is never shown to an agent. See [spec-phase-2](../design/spec-phase-2.md) and [ADR 0008](0008-sync-modes.md).
- Exit codes: 2 caller must fix, 3 round running, 4 only the user can confirm, 5 show the review again, 1 other.
- Rounds started by an agent run detached (systemd, or a child in dev mode) and the call waits up to a limit, so a client timeout never kills rclone.
- `Ops::copy` and `Ops::move` (`copy`, `move` and `transfer <id>` on the CLI; `copy`, `move` and `show_transfer` over MCP) take a file or folder on this computer or `account:path` on each side. The destination is the new name and must not exist: it is never overwritten or merged (exit 2). A folder moves with `rclone moveto` inside one account, a rename on the server, and with `rclone move` otherwise, which deletes the source only after it transferred without error. `--dry-run` lists files and bytes from `lsjson` and runs no transfer.
- A transfer runs detached like a round: `Ops` writes a job file (`transfers/<id>.json`), a child process (a transient `systemd-run` unit on the host inside the Flatpak, which ends with its main process) runs the hidden `transfer-run <id>` and holds `transfer-<id>.lock` while rclone runs, and the call waits up to `--wait` seconds and reports `finished: false` with the id when it is still going. A job whose lock is gone without an outcome reads as failed. A transfer that fails within the wait is an error (exit 1); read later, it is a `failed` status with the error.
- Refusals before anything is touched: source that is missing, is an account root or is or contains the folder of a sync, or contains the destination (exit 2); source or destination inside the folder of a sync with a running round (exit 3). Moving inside a synced folder is allowed and the next round carries it over; the `too_many_deletes` gate holds a mass deletion for the user.
- The GUI reads the config and every state file on each poll, so what an agent does shows up within seconds. `syncfast theme` writes `gui.json` and sends `reload-themes` to the window.

## Consequences
- The deletion guard is a behavioural limit, not a sandbox: an agent runs as the user and could write the state files itself. It prevents accidents and makes the intended path safe.
- New operations are added to `ops` once and exposed twice; the CLI text output and the MCP tool schemas are the only per-frontend work.
- `rmcp` adds a dependency tree (`schemars`, `chrono`, `futures`) and follows the MCP spec's pace.
