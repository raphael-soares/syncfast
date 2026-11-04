# Product

## What syncfast is

A small Linux desktop app that keeps folders on this computer and folders on Google Drive in sync, and backs up the folders you choose. A tray-resident window sets it up and shows what happened. Sync is done by rclone (`bisync` by default, one-way copy or mirror per sync), backups by restic, both scheduled by systemd user units that syncfast generates.

It is for one person with one or more Drive accounts who wants a folder to just stay in sync, wants to see what is about to change before anything is lost, and wants a versioned copy to go back to.

## What it is not

- Not a Drive mount or a virtual drive. Files live on disk and sync in rounds.
- Not a backup service. Backups go to a Drive account or a disk you already have, encrypted with a key that only you keep.
- Not a multi-cloud client. Google Drive only, through rclone.
- Not a sharing or collaboration tool. It does not manage permissions or links, and it skips Google Docs files.
- Not a service. There is no syncfast account, server or telemetry.
- Not available outside Linux or outside Flatpak.

## Principles

1. **Sync safety first.** rclone can destroy data on both sides, so the flags are fixed in one place and a review gates anything risky: the first sync, changed filters, a changed mode, mass deletions, critical errors. How much of that review syncfast may confirm on its own is a setting (`manual`, `deletions`, `automatic`). At the default level syncfast confirms it itself when the preview deletes nothing, except the first sync of a folder, which always waits for you; an empty source and a critical error always stop for the user. See [ADR 0002](docs/adr/0002-bisync-safety-invariants.md) and [ADR 0008](docs/adr/0008-sync-modes.md).
2. **Agents can do what users do, except sign in and lower safety.** Every action in the window exists as a CLI command with `--json` and as an MCP tool. Creating accounts, changing credentials, OAuth, the backup key and any setting that makes data loss easier stay with the user, and an agent cannot confirm a review that deletes files. See [ADR 0007](docs/adr/0007-agent-interface.md).
3. **Flatpak is the only channel.** One install path on every distribution, with rclone and restic bundled and nothing from the host but `flatpak` and systemd. Only releases reach users. See [ADR 0006](docs/adr/0006-flatpak-packaging.md).
4. **Local first, no surprises.** Your files stay where you put them, the app says what it is doing, and an error is shown next to the thing it is about.
5. **Small and quiet, configurable underneath.** One window, one tray icon, English text. Every behavior has a sane default that needs no setup, and every one of them can be changed globally or per sync.
