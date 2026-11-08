# Troubleshooting

## A sync says Review needed

syncfast stops a sync and waits for you when the first sync is pending, the filters changed, many files would be deleted, every file of one side changed at once, or rclone hit a critical error. Open the sync and press **Review…**. The preview is a dry run and may list deletions the real round would not do. Confirm only when the list is what you expect.

How much waits for you is the **Review** option. At the default, **Ask only for deletions**, syncfast confirms a review itself when its preview deletes nothing (the first sync of a folder always waits). **Ask every time** never confirms by itself; **Never ask** also confirms deletions. The Activity tab marks a round confirmed that way as **Reviewed automatically**.

## Many files would be deleted

By default, a round that would delete more than half of the files is refused (the limit is **Stop when more than this share would be deleted** in the **Options** tab). This usually means a folder is empty, unmounted or moved. Check the local folder and the Drive folder, then review. **Delete and Sync** goes ahead; to undo a mass delete on Drive, use the Drive trash.

## Signed out

The account shows **Signed out. Reconnect to resume syncing.** Reconnect from **Settings…**, **Accounts**. If it happens every week, the OAuth consent screen is still in **Testing**; publish it ([Google Drive](google-drive.md)).

## Offline

With no network the status reads **Offline** and syncfast retries by itself. It sends no notification for that.

## A backup says Needs attention

A backup stops by itself, and does not delete anything, when it cannot run safely. The card at the top of the backup says why and offers the button that fixes it. After the fix, press **Back Up Now**.

| The card says | What to do |
|---|---|
| Google Drive refused the account. | Press **Reconnect Account…**, or reconnect in **Settings…**, **Accounts**. |
| Google Drive is full. | Free space on Drive, then press **Back Up Now**. |
| No permission to the backup folder on Google Drive. | The account can no longer write to the destination folder. Check the folder's sharing on Drive. |
| The backup folder is missing. | The destination folder was moved or deleted on Drive, or the disk shows an empty folder. Restore it, then press **Back Up Now**. |
| `<destination>` is not available. | Plug in the disk. This appears when it has been away much longer than the schedule. |
| The key for this backup is missing on this computer. | Press **Enter Key…** and type the key you saved. |
| `<folder>` is missing. | The folder was moved, renamed or is on an unmounted disk. Put it back, or remove it from **Folders** in the **Settings** tab. No snapshot is made while a folder is missing. |
| `<folder>` is empty. | If the folder is on a disk, mount it. An empty folder is never backed up, so an unmounted disk cannot replace good snapshots. |
| Backups have failed since `<date>`: … | Open the **Activity** tab and read the last lines of a failed run. |
| Old snapshots could not be cleaned up: … | The backups work, but retention could not delete old data. Read the reason; it is tried again after the next backup. |
| The check found damaged data. | See [Backups are paused after a check](#backups-are-paused-after-a-check). |
| A message from the configuration | The backup is invalid, for example a destination that now overlaps a sync. Fix it in the **Settings** tab. |

## Key storage is not available

syncfast keeps each backup key in your keyring. It asks the Secret portal first. If there is no Secret portal, it asks the Secret Service of your session (`org.freedesktop.secrets`). If neither answers, no backup can be created.

On Hyprland the portal does not use gnome-keyring for secrets by default. Add this line under `[preferred]` in `~/.config/xdg-desktop-portal/hyprland-portals.conf` (or in `portals.conf`):

```text
org.freedesktop.impl.portal.Secret=gnome-keyring
```

Then restart `xdg-desktop-portal` (or log out and in) and check it with:

```sh
busctl --user introspect org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.Secret
```

Without the portal, syncfast uses the host keyring and **Settings › Backups** says "Host keyring". That works, but it lets the Flatpak read every secret in your keyring. The portal is safer.

After the change, open **Settings… › Backups** and press **Test Key Storage**: it should say **Secret portal**. If your keyring asks for a password and nobody answers, a scheduled backup waits 30 seconds and then needs attention; unlock the keyring and press **Back Up Now**. More in [Backing up with syncfast](backup.md#key-storage).

## Backups are paused after a check

A check found damaged data in the backup: the card says **The check found `<n>` damaged files in the backup. Backups and cleanup are paused.** syncfast stops there on purpose, because a new backup on top of a damaged index could spread the damage. Checks still work and nothing already stored is touched.

Press **Check Again**, which runs a **Full Check** (it reads all the data; on Drive that means downloading it and can take hours). The pause ends only when a full check finds no errors; a sample check without errors does not prove the damage is gone.

If the full check still finds errors, syncfast does not repair the backup. Use **Browse Files…** and **Restore To…** to copy out what you need from the snapshots that still read, then create a new backup in a new destination folder. For the restic side, see <https://restic.readthedocs.io/en/stable/077_troubleshooting.html>.

## Nothing syncs after a restart

The syncs run as systemd user units that call `flatpak run`. Check them:

```sh
systemctl --user list-timers 'syncfast-*'
systemctl --user status syncfast-watch.service
journalctl --user -u syncfast-watch.service -n 50
```

Opening syncfast rewrites the units. The units need `flatpak` on the user systemd `PATH`.

## Real-time syncing stopped after a configuration error

After fixing a configuration error, open syncfast or run `syncfast apply` to restart real-time watching.

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
