# syncfast for AI agents

syncfast keeps folders on this computer in sync with a cloud account (Google Drive, through rclone bisync). An agent can do everything the user can do, except sign in: creating an account, changing its credentials and OAuth are for the user.

Every command takes `--json` and then prints one JSON value (errors too: `{"error": {"code": ..., "message": ...}}`). `syncfast mcp` serves the same operations as MCP tools, so a command and its tool give the same answer.

## Workflow

1. **Explore.** Look before proposing anything.
   - `syncfast accounts` lists the accounts (name and type, never secrets).
   - `syncfast folders gdrive:` lists the folders of a remote folder; `syncfast explore gdrive:Notes --depth 2` and `syncfast explore ~/Notes` summarize a folder: files, size, common extensions and newest date per subfolder.
   - Explore reads names, sizes and dates only, never the content of a file. Output is capped (`--max-folders`, default 60); explore a subfolder to go deeper.
2. **Propose.** Tell the user which local folder pairs with which remote folder and which filters you suggest. `syncfast filters test --local ~/Notes --rule '- *.bak'` says how many files a rule leaves out (`x of y files excluded`) before anything is saved. Presets: `system_junk` and `dev_dirs`.
3. **Add.** `syncfast add --name Notes --local ~/Notes --remote gdrive:Notes`. Use `--create-local` and `--create-remote` only when the user agreed to create folders. The new sync is held for the review of its first round: nothing is copied yet.
4. **Review.** `syncfast review show notes` dry-runs the first sync and lists what would be uploaded, downloaded, replaced or deleted, with a `filters_hash`.
   - When `agent_can_confirm` is true (nothing is deleted): `syncfast review confirm notes --filters-hash <hash>` starts the sync.
   - When the review deletes files, confirm is refused (exit 4, `This review deletes files. Confirm it in syncfast.`). Tell the user to review it in the syncfast window.
5. **Follow.** `syncfast status` (all syncs) and `syncfast show notes` say where a sync stands. `syncfast sync notes` runs a round and waits up to `--wait` seconds (default 60; the round goes on by itself afterwards, so read `status` later). `syncfast conflicts list notes` and `syncfast conflicts resolve notes <path> --keep current|conflicted|both` settle files that changed on both sides. `syncfast activity notes` and `syncfast log notes <round>` show what rounds did.

Other commands: `remove <id> [--delete-versions]`, `pause <id>|--all`, `resume <id>|--all`, `filters set <id> [--preset N]... [--no-presets] [--rule R]... [--clear-rules]`, `mkdir remote:folder`, `theme [name]`, `apply`, `agent-guide`.

## Safety rules

- A confirmation needs the `filters_hash` of the review you showed. It is refused (exit 5) when no review was shown, when the hash is not the one shown, or when the filters changed since. Show the review again.
- Changing filters holds the sync for a new review. A sync held because many files would be deleted (`too_many_deletes`) is for the user to confirm.
- There is no flag to confirm a review that deletes files. Do not work around the refusal by editing files in syncfast's state folder.
- Removing a sync leaves the files on both sides; `--delete-versions` also deletes the saved copies of replaced local files.
- Never ask for, print or store credentials. `accounts` shows none.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | done |
| 1 | the operation failed (rclone, files, systemd) |
| 2 | wrong request or config; the message says what to fix |
| 3 | a round of this sync is running; try again when it finishes |
| 4 | the review deletes files: only the user can confirm it |
| 5 | no matching review was shown; run `review show` again |

## MCP

`syncfast mcp` speaks MCP over stdio. Tools: `status`, `show_sync`, `add_sync`, `remove_sync`, `pause_sync`, `resume_sync`, `set_filters`, `test_filters`, `list_accounts`, `list_remote_folders`, `create_remote_folder`, `explore`, `sync_now`, `show_review`, `confirm_review`, `list_conflicts`, `resolve_conflict`, `list_activity`, `show_round_log`, `theme`. Errors come back as tool results with `isError` set and the message of the CLI.

Claude Code:

```sh
claude mcp add syncfast -- syncfast mcp
```

omp (`~/.omp/agent/mcp.json`):

```json
{ "mcpServers": { "syncfast": { "command": "syncfast", "args": ["mcp"] } } }
```

Flatpak install (the command lives inside the sandbox):

```sh
claude mcp add syncfast -- flatpak run --command=syncfast io.github.raphael_soares.syncfast mcp
```

```json
{ "mcpServers": { "syncfast": { "command": "flatpak", "args": ["run", "--command=syncfast", "io.github.raphael_soares.syncfast", "mcp"] } } }
```
