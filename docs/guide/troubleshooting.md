# Troubleshooting

## A sync says Review needed

syncfast stops a sync and waits for you when the first sync is pending, the filters changed, many files would be deleted, every file of one side changed at once, or rclone hit a critical error. Open the sync and press **Review…**. The preview is a dry run and may list deletions the real round would not do. Confirm only when the list is what you expect.

## Many files would be deleted

A round that would delete more than half of the files is refused. This usually means a folder is empty, unmounted or moved. Check the local folder and the Drive folder, then review. **Delete and Sync** goes ahead; to undo a mass delete on Drive, use the Drive trash.

## Signed out

The account shows **Signed out. Reconnect to resume syncing.** Reconnect from **Settings…**, **Accounts**. If it happens every week, the OAuth consent screen is still in **Testing**; publish it ([Google Drive](google-drive.md)).

## Offline

With no network the status reads **Offline** and syncfast retries by itself. It sends no notification for that.

## Nothing syncs after a restart

The syncs run as systemd user units that call `flatpak run`. Check them:

```sh
systemctl --user list-timers 'syncfast-*'
systemctl --user status syncfast-watch.service
journalctl --user -u syncfast-watch.service -n 50
```

Opening syncfast rewrites the units. The units need `flatpak` on the user systemd `PATH`.

## No tray icon

The tray uses the StatusNotifier protocol. Some desktops (GNOME without an extension) have no tray. The window still works; keep it open.

## Find my files

| What | Where |
|---|---|
| syncfast config | `~/.config/syncfast/config.toml` |
| rclone accounts | `~/.config/rclone/rclone.conf` |
| Sync state and round logs | `~/.local/state/syncfast/` |
| Saved versions of replaced files | `~/.local/share/syncfast/versions/<sync id>/` |

A round's raw log is `~/.local/state/syncfast/pairs/<sync id>/runs/<date>.jsonl`. Remove tokens and secrets before sharing a log, and never paste the output of `rclone config dump`.

## Reporting a bug

Open an [issue](https://github.com/raphael-soares/syncfast/issues/new/choose) with the syncfast version (`flatpak info io.github.raphael_soares.syncfast`), what you did, and the relevant lines of the round log or journal.
