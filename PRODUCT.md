# Product

## What syncfast is

A small Linux desktop app that keeps folders on this computer equal to folders on Google Drive, in both directions. A tray-resident window sets it up and shows what happened. The work is done by `rclone bisync`, scheduled by systemd user units that syncfast generates.

It is for one person with one or more Drive accounts who wants a folder to just stay in sync and wants to see what is about to change before anything is lost.

## What it is not

- Not a Drive mount or a virtual drive. Files live on disk and sync in rounds.
- Not a backup tool. Sync copies deletions and edits. Replaced local files are kept for 30 days, and Drive has its own trash and revisions.
- Not a multi-cloud client. Google Drive only, through rclone.
- Not a sharing or collaboration tool. It does not manage permissions or links, and it skips Google Docs files.
- Not a service. There is no syncfast account, server or telemetry.
- Not available outside Linux or outside Flatpak.

## Principles

1. **Bisync safety first.** `rclone bisync` can destroy data on both sides, so the flags are fixed in one place and a review gates anything risky: the first sync, changed filters, mass deletions, critical errors. A gated sync runs again only after the user sees a dry run and confirms. See [ADR 0002](docs/adr/0002-bisync-safety-invariants.md).
2. **Agents can do what users do, except sign in.** Every action in the window exists as a CLI command with `--json` and as an MCP tool. Creating accounts, changing credentials and OAuth stay with the user, and an agent cannot confirm a review that deletes files. See [ADR 0007](docs/adr/0007-agent-interface.md).
3. **Flatpak is the only channel.** One install path on every distribution, with rclone bundled and nothing from the host but `flatpak` and systemd. Only releases reach users. See [ADR 0006](docs/adr/0006-flatpak-packaging.md).
4. **Local first, no surprises.** Your files stay where you put them, the app says what it is doing, and an error is shown next to the thing it is about.
5. **Small and quiet.** One window, one tray icon, English text, no settings that the default behavior does not need.
