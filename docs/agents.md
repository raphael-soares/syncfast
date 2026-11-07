# syncfast for AI agents

syncfast keeps folders on this computer in sync with a cloud account (Google Drive, through rclone bisync). An agent can do everything the user can do, except sign in: creating an account, changing its credentials and OAuth are for the user.

Every command takes `--json` and then prints one JSON value (errors too: `{"error": {"code": ..., "message": ...}}`). `syncfast mcp` serves the same operations as MCP tools, so a command and its tool give the same answer.

## Workflow

1. **Explore.** Look before proposing anything.
   - `syncfast accounts` lists the accounts (name and type, never secrets).
   - `syncfast folders gdrive:` lists the folders of a remote folder; `syncfast explore gdrive:Notes --depth 2` and `syncfast explore ~/Notes` summarize a folder: files, size, common extensions and newest date per subfolder.
   - Explore reads names, sizes and dates only, never the content of a file. Output is capped (`--max-folders`, default 60); explore a subfolder to go deeper.
2. **Propose.** Tell the user which local folder pairs with which remote folder and which filters you suggest. `syncfast filters test --local ~/Notes --rule '- *.bak'` says how many files a rule leaves out (`x of y files excluded`) before anything is saved. Presets: `system_junk`, `dev_dirs` and `git_repos`. `git_repos` leaves out every folder that holds a `.git`, including repos created later; `filters test` and `review show` list them.
3. **Add.** `syncfast add --name Notes --local ~/Notes --remote gdrive:Notes`. Use `--create-local` and `--create-remote` only when the user agreed to create folders. The new sync is held for the review of its first round: nothing is copied yet.
4. **Review.** `syncfast review show notes` dry-runs the first sync and lists what would be uploaded, downloaded, replaced or deleted, with a `filters_hash`.
   - When `agent_can_confirm` is true (nothing is deleted): `syncfast review confirm notes --filters-hash <hash>` starts the sync.
   - When the review deletes files, confirm is refused (exit 4, `This review deletes files. Confirm it in syncfast.`). Tell the user to review it in the syncfast window.
5. **Follow.** `syncfast status` (all syncs) and `syncfast show notes` say where a sync stands. `syncfast sync notes` runs a round and waits up to `--wait` seconds (default 60; the round goes on by itself afterwards, so read `status` later). `syncfast conflicts list notes` and `syncfast conflicts resolve notes <path> --keep current|conflicted|both` settle files that changed on both sides. `syncfast activity notes` and `syncfast log notes <round>` show what rounds did.
6. **Organize.** `syncfast copy <source> <destination>` and `syncfast move <source> <destination>` copy or move a file or folder, each side on this computer (`~/Notes`) or `account:path`, in any of the four directions. The destination is the new file or folder itself (`move ~/Inbox gdrive:Archive/Inbox` makes `Archive/Inbox`), not the folder it goes into.
   - Run with `--dry-run` first and show the user the files and bytes it reports; it changes nothing on either side. Run `move` only when the user asked for it.
   - A move inside one account is a rename on the server (`server_side: true`), with nothing downloaded. Between accounts or this computer and a remote, rclone transfers first and deletes the source only after it transferred without error; a failed move leaves what was not transferred where it was.
   - The transfer runs detached, so a client timeout never kills it. The command waits up to `--wait` seconds (default 60). When it is still going the answer has `finished: false` and an `id`: `syncfast transfer <id> [--wait N]` reads it later (`running`, `done` or `failed` with the error). A transfer that fails within the wait exits 1.
   - The destination must not exist: it is never overwritten or merged (exit 2). Also refused with exit 2: a source that does not exist, is the root of an account (`gdrive:`), or is or contains the folder of a sync (moving it breaks the pair: `remove` the sync first), and a destination inside the source. Exit 3 when the source or destination is inside the folder of a sync whose round is running.
   - Changing files inside a synced folder is allowed; the next round carries the change to the other side. When it turns into a mass deletion the `too_many_deletes` gate holds the sync for the user.
7. **Tune.** `syncfast settings --sync notes` lists how a sync behaves: every key with its `value`, its `origin` (`builtin`, `defaults` or `sync`), the `default` it falls back to and `applies`; `lowered_safety` names the keys less safe than the built-in default. Without `--sync` it lists the defaults every sync inherits.
   - `syncfast settings set <key> <value> [--sync notes]` changes one (`debounce 30s`, `max_delete 20`, `versions true`); without `--sync` it writes `[defaults]`. `syncfast settings reset <key> [--sync notes]` removes the sync's own value, or the changed default. Keys: `watch`, `debounce`, `max_wait`, `interval`, `conflict_resolve`, `conflict_loser`, `versions`, `versions_keep_days`, `max_delete`, `resync_mode`, `empty_dirs`. A wrong key, value or combination exits 2 with the message of the config.

Other commands: `remove <id> [--delete-versions]`, `pause <id>|--all`, `resume <id>|--all`, `filters set <id> [--preset N]... [--no-presets] [--rule R]... [--clear-rules]`, `mkdir remote:folder`, `theme [name]`, `apply`, `agent-guide`.

## Safety rules

- A confirmation needs the `filters_hash` of the review you showed. It is refused (exit 5) when no review was shown, when the hash is not the one shown, or when the filters changed since. Show the review again.
- Changing filters holds the sync for a new review. A sync held because many files would be deleted (`too_many_deletes`) is for the user to confirm.
- Settings that lower safety are for the user: a larger `max_delete`, `versions` from true to false and `conflict_loser = delete`, on a sync or in the defaults. Setting one, or resetting a key to a value that is less safe, is refused (exit 4, `This setting lowers safety (<keys>). Change it in syncfast.`) and nothing is written. Lowering `max_delete` or changing a value to a safer one is allowed.
- There is no flag to confirm a review that deletes files. Do not work around the refusal by editing files in syncfast's state folder.
- Removing a sync leaves the files on both sides; `--delete-versions` also deletes the saved copies of replaced local files.
- `copy` and `move` never overwrite or merge, and never touch a folder a running round is working on. Do not work around a refusal with `rclone` or shell commands: they skip these checks.
- Never ask for, print or store credentials. `accounts` shows none.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | done |
| 1 | the operation failed (rclone, files, systemd) |
| 2 | wrong request or config; the message says what to fix |
| 3 | a round of this sync is running (also for `copy` and `move` inside its folders); try again when it finishes |
| 4 | only the user can do this: confirm a review that deletes files, or lower a safety setting |
| 5 | no matching review was shown; run `review show` again |

## MCP

`syncfast mcp` speaks MCP over stdio. Tools: `status`, `show_sync`, `add_sync`, `remove_sync`, `pause_sync`, `resume_sync`, `set_filters`, `test_filters`, `list_accounts`, `list_remote_folders`, `create_remote_folder`, `explore`, `copy`, `move`, `show_transfer`, `sync_now`, `show_review`, `confirm_review`, `list_conflicts`, `resolve_conflict`, `list_activity`, `show_round_log`, `theme`, `get_settings`, `set_setting`, `reset_setting`. Errors come back as tool results with `isError` set and the message of the CLI. `copy` and `move` take `source`, `destination`, `dry_run` and `wait_seconds` and answer like the commands; `show_transfer` takes the `id`. `get_settings` takes an optional `sync`; `set_setting` takes an optional `sync`, `key` and `value` (a JSON boolean, number or string); `reset_setting` takes an optional `sync` and `key`.

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
