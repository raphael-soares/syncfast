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
| The saved key does not open this backup. | The key on this computer is not the one of this backup. Press **Enter Key…** and type the right one. |
| `<folder>` is missing. | The folder was moved, renamed or is on an unmounted disk. Put it back, or remove it from **Folders** in the **Settings** tab. No snapshot is made while a folder is missing. |
| `<folder>` is empty. | If the folder is on a disk, mount it. An empty folder is never backed up, so an unmounted disk cannot replace good snapshots. |
| The account of this backup was disconnected. | The Google Drive account no longer exists in syncfast. Add it again in **Settings…**, **Accounts**, then press **Back Up Now**. |
| Backups keep failing. Backups have failed since `<date>`: … | Open the **Activity** tab and read the last lines of a failed run. |
| Old snapshots could not be cleaned up: … | The backups work, but retention could not delete old data. Read the reason. The cleanup is tried again after every backup; the card shows after the third failure in a row and goes away when one works. |
| The check found damaged data. Backups are paused until you review it. | See [Backups are paused after a check](#backups-are-paused-after-a-check). |
| A message from the configuration | The backup is invalid, for example a destination that now overlaps a sync. Fix it in the **Settings** tab. |

## Backups are paused after a check

Each month a backup reads a sample of the data it stored, and after a series of failed backups it checks the structure of the repository. A check can also be run by hand from the **...** menu, **Check Now**: **Quick Check** reads no data, **Check Sample** reads about 5%, and **Full Check…** reads everything and can take hours on Google Drive.

When a check finds damaged data, the backup shows **Needs attention** with the number of damaged files. syncfast then stops backing up and cleaning old snapshots, because a new snapshot on top of a damaged pack would spread the damage. Nothing is deleted.

1. Read the [restic troubleshooting guide](https://restic.readthedocs.io/en/stable/077_troubleshooting.html). syncfast never runs `restic repair`; guided repair is planned for a later phase.
2. Find the cause. A full disk, a drive that was unplugged during a backup and a bad sector are common. For a Google Drive backup, a damaged pack is usually a network problem during an upload.
3. After the repair, press **Check Again**. It runs a full check, and the pause ends only when that check finds nothing. A quick check or a sample that finds nothing does not end it.

The **Activity** tab lists every check with its kind and result (`Full check · 2 errors`) and keeps the log.

A backup that says it is in use by another process found a lock from another run of restic, for example a backup from another computer. It waits and tries again by itself. A lock left by a run that died is removed automatically.

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

Without the portal, syncfast uses the host keyring and **Settings > Backups** says "Host keyring". That works, but it lets the Flatpak read every secret in your keyring. The portal is safer.

After the change, open **Settings > Backups** and press **Test Key Storage**: it should say **Secret portal**. If your keyring asks for a password and nobody answers, a scheduled backup waits 30 seconds and then needs attention; unlock the keyring and press **Back Up Now**. More in [Backing up with syncfast](backup.md#key-storage).

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
