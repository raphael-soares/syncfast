# 0007: Interface for AI agents

## Context
The owner wants AI agents to do everything a user does in syncfast, except sign in, and to explore Drive and local folders to help decide what to sync. The GUI has its logic in its backend, where a CLI or a server cannot reach it.

## Decision
- `syncfast-core::ops` holds one function per operation (`Ops::status`, `add`, `show_review`, `confirm_review`, `explore`...) that returns `Serialize` structs. The CLI (`--json` on every subcommand) and `syncfast mcp` only translate arguments and print. The MCP server uses the official `rmcp` SDK (it passes `cargo deny`).
- Authentication stays with the user: no operation creates an account, changes credentials or runs OAuth. `accounts` lists names, types and the shared client ID warning, never a secret.
- Exploring reads metadata only (`rclone lsjson -R`): names, sizes, dates, counts per folder and extension. Output is capped by depth and by a maximum number of folders.
- A held sync is released by a review. `review show` writes what it showed to `pairs/<id>/preview.json`. `review confirm` accepts it only when the given `filters_hash` is the shown one, the filters have not changed since, the gate still waits for that kind of review, and the review has no `DeleteLocal` or `DeleteRemote`. Otherwise nothing changes: exit 4 with `This review deletes files. Confirm it in syncfast.` for deletions, exit 5 for a review that was not shown or is stale. There is no flag around it.
- Exit codes: 2 caller must fix, 3 round running, 4 only the user can confirm, 5 show the review again, 1 other.
- Rounds started by an agent run detached (systemd, or a child in dev mode) and the call waits up to a limit, so a client timeout never kills rclone.
- The GUI reads the config and every state file on each poll, so what an agent does shows up within seconds. `syncfast theme` writes `gui.json` and sends `reload-themes` to the window.

## Consequences
- The deletion guard is a behavioural limit, not a sandbox: an agent runs as the user and could write the state files itself. It prevents accidents and makes the intended path safe.
- New operations are added to `ops` once and exposed twice; the CLI text output and the MCP tool schemas are the only per-frontend work.
- `rmcp` adds a dependency tree (`schemars`, `chrono`, `futures`) and follows the MCP spec's pace.
