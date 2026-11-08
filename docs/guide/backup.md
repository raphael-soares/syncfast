# Backing up with syncfast

This guide is also at <https://github.com/raphael-soares/syncfast/blob/main/docs/guide/backup.md>.

A sync keeps two folders equal, so a deleted or damaged file is deleted or damaged on both sides. A backup keeps history: every run adds a snapshot of your folders, encrypted, to a destination you choose, and older snapshots stay until the retention policy lets them go. syncfast uses [restic](https://restic.net/) for this, through the same rclone accounts as your syncs.

A backup never deletes your files and has no review step. A folder that is missing or empty is never turned into a snapshot; the backup stops and says so.

## Create a backup

Press **+** next to **Backups** in the sidebar (**New Backup…**). The sheet has these steps:

1. **Folders**: **Add Folder…** for each folder to back up. Folders cannot be inside each other. Folders mounted from another disk inside them are not included; add such a disk as its own folder.
2. **Destination**: **Google Drive** or **This Computer**. For Drive, pick the account and a folder (**New Folder…**); for this computer, **Choose…** a folder, for example on an external disk.
3. **Schedule**: **Every hour**, **Every day** (the default) or **Every week**, and how many snapshots to keep (see [Retention](#retention)). **Advanced** holds **Stay on one file system**, **Upload limit** and **Download limit**.
4. **Filters**: what to leave out ([Filters](#filters)).
5. **Name**: the name in the sidebar. Its id (`Backup id: …`) is made from the name.
6. **Your Backup Key**: the key is shown once ([The key](#the-key)). Nothing is created until you press **I Saved the Key**.

After that syncfast creates the destination, schedules the backup and runs the first one. The backup shows **Backing up…** with a progress bar, then **Up to date**. **Back Up Now** runs one at any time and **Pause** stops the schedule until you press **Resume**.

The schedule is a systemd user timer: every day at 03:00 (every week, on Monday at 03:00; every hour, on the hour), with a random delay of up to 10 minutes. If the computer was off at that time, the run happens when it is back. With no network, a Google Drive backup shows **Offline** and tries again about 15 minutes later, with no notification.

## The key

Everything in the destination is encrypted with a key that syncfast generates for each backup: 52 letters and digits, shown in 13 groups of 4. **syncfast cannot recover it, and without it the backup cannot be read**, by anyone. Keep it somewhere safe that is not this computer: a password manager or a printed sheet. **Save Key to File…** writes `<id>-backup-key.txt`, readable only by you and not encrypted; move it off the computer and delete it.

You can see the key again from the **…** menu of a backup: **Show Key…** asks for confirmation first, because anyone who sees the key can read your backed-up files.

syncfast stores the key in your desktop's keyring, never in a file, a log or a command line. See [Key storage](#key-storage).

### Backups created by an agent

An AI agent can create a backup through the command line or MCP ([agent guide](../agents.md)). syncfast generates and stores the key inside that process and never hands it to the agent. The backup runs, and syncfast shows **Save the backup key** at the top of the backup, with a notification the first time. Press **Show Key…**, write the key down and press **I Saved the Key**. Until then you cannot restore on another computer.

An agent cannot lower the safety of a backup: it cannot reduce any **Keep** value, remove a folder from the backup, remove the backup, change its destination or turn off **Stay on one file system**. Those changes are yours to make in syncfast.

## Destination

The destination holds one backup only. It must be new or empty, and:

- outside the remote folder and the local folder of every sync,
- not inside a folder you back up (for a folder on this computer),
- not used or contained by another backup.

The sheet shows the problem when a folder breaks one of these. Choose a folder that only this backup uses. With the Flatpak, a folder on this computer must be under your home folder, `/run/media` or `/mnt`.

An external disk that is not plugged in shows **Destination not available**. Backups resume when it is back. If it stays away for much longer than the schedule (twice the period, at least 48 hours), the backup needs attention.

The destination is never changed after the backup is created: to move a backup, create a new one. Removing a backup (**Remove Backup…**) stops it and leaves everything at the destination; the key is deleted from this computer only if you tick **Also delete the key from this computer**.

## Retention

By default syncfast keeps 7 daily, 4 weekly and 6 monthly snapshots, and no yearly ones. **Customize…** in the Schedule step, or **Keep** in the **Settings** tab of the backup, sets **Daily**, **Weekly**, **Monthly** and **Yearly**; **Yearly** at 0 is off. At least one value must be above 0. The values count periods that have a snapshot: keeping 7 daily means the last 7 days that have a snapshot, not the last 7 calendar days.

After each backup that succeeded and read at least one file, syncfast deletes the snapshots outside the policy and frees their space at the destination. This is the only time snapshots are deleted.

- The policy covers every snapshot made by this machine in the repository, even after you change the backup's folders. Snapshots of the old folder list age out by the same rules.
- Snapshots made by other machines in the same repository are listed, with the machine name, and never deleted.
- Reducing any value, including **Yearly** to 0, asks **Delete older snapshots?** before saving. **Save and Delete Old Snapshots** goes ahead.
- When cleanup fails, the backup itself still counts as done and the detail says **Old snapshots could not be cleaned up** with the reason. After 3 failed cleanups in a row the backup needs attention.

## Bandwidth limits

**Upload limit** and **Download limit** (**Advanced** in the wizard, **Options** in the **Settings** tab) default to **Unlimited**. Set a speed as a whole number followed by `K`, `M` or `G`, in KiB, MiB or GiB per second, for example `500K` or `2M`. The limits are per backup, are not inherited from the sync limits, and apply to backing up, cleaning up, checking and restoring.

## Stay on one file system

On by default. A drive mounted inside a backed-up folder (an external disk, a Google Drive mounted with rclone) is left out, and so are Btrfs subvolumes. This keeps a backup from reading a whole cloud drive by mistake. Add such a folder as a folder of its own to include it. Turning the switch off backs up everything mounted inside your folders, so syncfast asks first (**Include mounted drives and subvolumes?**).

## Filters

The **Filters** step is the one from a sync's **Filters** tab, with two differences: a backup only excludes, and the default presets are different.

- **System junk files** (`system_junk`) is on by default.
- **Build artifacts and caches** (`build_artifacts`) is on by default. It leaves out what a tool recreates: `node_modules`, `target`, `.venv`, `venv`, `__pycache__`, `.gradle`, `.mypy_cache`, `.pytest_cache`, `.ruff_cache`, `.tox`, `.parcel-cache` and `.cache`. It keeps `.git`. Folders called `build` or `dist` are not part of it, because they often hold real work; add them as custom rules if yours only hold output.
- **Developer folders** (`dev_dirs`) is off by default. It leaves out every `.git` folder, so the history of your repositories would not be backed up.
- **Git repositories** (`git_repos`) is off by default. It leaves out every folder that holds a `.git`, whole repositories included.

Custom rules are one per line and start with `- ` (for example `- *.iso`). Lines starting with `+ `, and patterns with `{...}` or `[!...]`, are refused in a backup. **Test Filters** counts how many files and bytes your rules leave out, without uploading anything.

Changing filters does not ask for a review. They only affect future snapshots: files you left out stay in older snapshots until those expire.

## Checks

syncfast checks that the backup is readable:

- **Sample check**: once a month, after a successful backup, it reads 5% of the stored data and verifies it.
- **Quick check**: after a backup that failed for no known reason, once per series of failures. It verifies the structure of the backup (indexes, folders and snapshots) without reading the data, so it is cheap on Drive.
- **Full check**: only when you ask. It reads and verifies all the data, which for a Drive backup means downloading all of it and can take hours.

Run any of them from the **…** menu of a backup, **Check Now**: **Quick Check**, **Check Sample** or **Full Check…**. The result is in the **Activity** tab. A check that finds damaged data pauses the backup and its cleanup; see [A backup is paused after a check](troubleshooting.md#backups-are-paused-after-a-check).

## Restore files

Open the **Snapshots** tab of the backup. Each row is a snapshot with its date, files and size.

1. Press **Restore Snapshot…** to restore everything in it, or **Browse Files…** to open the snapshot, tick the files and folders you want (ticking a folder takes everything inside) and press **Restore To…**.
2. Choose a folder. syncfast creates a new folder inside it named `<backup name> <date and time>` (with ` 2`, ` 3`… if it exists) and confirms the exact folder: **Restore to …?**
3. Press **Restore**. A progress bar shows the files and bytes. Closing the window does not stop it. When it ends, **Open Folder** shows the result.

A restore never writes over anything and never goes into the original place: it always makes a new folder. Copy back what you need from there. A restore cannot go inside a folder of a sync that is running; try again when the round finishes.

## Restore on a new machine

You only need the destination and the key.

1. Install syncfast and, if the backup is on Google Drive, add the same account (**Settings…**, **Accounts**). For a disk, plug it in.
2. Press **…** next to **Backups**, **Add Existing Backup…**.
3. Pick the **Destination** folder that holds the backup, enter the **Backup key** (capitals, lowercase, spaces and dashes are all accepted) and a **Name**, and press **Add Backup**. **No backup was found in that folder.** and **That key does not open this backup.** say what to fix.

The backup is added paused and with no folders to back up. **Browse Files…** works at once. To make this machine back up to it too, press **Add Folders…** and resume it.

## Two machines, one repository

Each machine adds the same destination with its own backup entry (**Add Existing Backup…** on the second one) and its own folders. Snapshots carry the machine's name: retention on each machine only looks at its own snapshots and never deletes the others', and the **Snapshots** tab shows the name of any other machine. If one machine is cleaning up while the other backs up, the second run can find the repository busy; it waits and tries again.

Renaming a machine makes its older snapshots look like another machine's: they are kept, and no longer cleaned up by this machine's policy.

## Key storage

syncfast asks for a place to keep keys when you create a backup, and refuses to create one without it. It looks, in order:

1. **Secret portal** (`org.freedesktop.portal.Secret`): the preferred one. The key lives in an encrypted file only syncfast can open.
2. **Host keyring** (Secret Service, `org.freedesktop.secrets`: GNOME Keyring, KWallet), when there is no portal. This works, but the Flatpak can then read every secret in your keyring.

**Settings… › Backups** says which one is in use under **Key storage**, and **Test Key Storage** writes, reads and deletes a test item to prove it works. When neither is available the sheet shows **Cannot reach the key storage** and **Continue** stays disabled.

### Enable the Secret portal on Hyprland

Hyprland's portal configuration does not route secrets to gnome-keyring by default. Add this line under `[preferred]` in `~/.config/xdg-desktop-portal/hyprland-portals.conf` (or in `portals.conf`):

```ini
org.freedesktop.impl.portal.Secret=gnome-keyring
```

Restart `xdg-desktop-portal` (or log out and in) and check it:

```sh
busctl --user introspect org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop org.freedesktop.portal.Secret
```

Then press **Test Key Storage**: it should say **Secret portal**. A backup created with the host keyring keeps finding its key after the portal appears, and the other way round.

## Common errors

Status of a backup in the sidebar and detail:

| Status | Meaning |
|---|---|
| **Backing up…**, **Checking…** | A run is in progress. |
| **Up to date** | The last backup worked. |
| **Needs attention** | Something needs you; the card says what and has the button that fixes it. |
| **Save the key** | An agent created this backup and you have not saved its key yet. |
| **Paused** | You paused it, or it was added with **Add Existing Backup…** and has no folders. |
| **Offline** | No network; it retries by itself. |
| **Destination not available** | The disk is not plugged in; it resumes when it is back. |
| **Backup failing** | The last 3 backups or more failed in a row. |
| **Will retry** | The last backup failed; it tries again. |

[Troubleshooting](troubleshooting.md) has the fix for **Needs attention**, key storage errors and a backup paused after a check. Bug reports go to <https://github.com/raphael-soares/syncfast/issues/new/choose>.
