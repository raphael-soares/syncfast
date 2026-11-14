# Getting started

## Install

```sh
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user https://raphael-soares.github.io/syncfast/io.github.raphael_soares.syncfast.flatpakref
flatpak run io.github.raphael_soares.syncfast
```

Update with `flatpak update`. To start syncfast hidden in the tray at login, run `flatpak run io.github.raphael_soares.syncfast --start-hidden` from your session's autostart. Use the absolute path to `flatpak` if your session's `PATH` does not include it.

The window closes to the tray: syncfast keeps running so syncs continue. Use **Quit syncfast** in the tray menu to stop it.

## Connect Google Drive

Press **Get Started**. syncfast needs a Google client of your own; the **Connect Google Drive** sheet walks through creating it and reads the file Google gives you ([Google Drive](google-drive.md) has the details). After you press **Connect**, finish signing in in your browser. syncfast then shows the account by its Google address.

## Create a sync

1. Press **+** next to **Syncs**, or continue from the welcome screen. The **New Sync** sheet opens. You can also drop a folder on it.
2. Pick the **Folder on this computer** with **Choose…**, or type it and create it.
3. **Folder on Google Drive** takes the same name as that folder; change it, leave it empty for the entire Drive, or **Browse…** your folders. A folder that does not exist yet is created on Drive. With more than one account, pick it first.
4. Under **Keep**, choose **Both the same** (changes and deletions go both ways) or **A copy on Google Drive** (files go up; nothing on this computer is changed or deleted).
5. Press **Create Sync**. **Options** holds the filters and the sync's name if you want to change them.

Pick folders that are not inside each other: two syncs cannot overlap, and a synced folder cannot contain syncfast's versions folder.

## Review the first sync

A new sync is held until you review it. **Review changes** shows, from a dry run, what would be uploaded, downloaded, replaced, renamed or deleted on each side. Nothing is copied before you press **Sync**. When the review would delete files, the button reads **Delete and Sync**; read that list first. A review after an error can bring back files you deleted or renamed since the last sync; they are listed under **Would come back**.

Afterwards the status reads **Up to date** and syncfast runs a round seconds after files change on this computer, within a minute of a change on Drive, after the computer wakes or the network comes back, and as a safety net every hour by default. **Sync Now** runs one immediately. Change these in the **Options** tab of a sync, or for all syncs in **Settings › Defaults**.

## Day to day

- **Settings › Advanced** lists every round of the sync you pick, with its counts. Expand one for the file list. Renaming a file or folder moves it on the other side and is counted as renamed, never as deleted.
- **Recently Deleted** lists what a sync deleted or replaced that can still come back: the versions kept on this computer, the versions a mirror to Drive keeps on the account, and the files in the Drive trash. **Restore** copies one back into the sync's folder and never overwrites: when the path is taken, the file comes back with `(restored <date>)` in its name. A mirror of Drive keeps nothing it deletes, so it has no such tab.
- **Conflicts** appears when a file changed on both sides. Choose **Keep Current**, **Keep Conflicted Copy** or **Keep Both**.
- **Filters** keeps files out of the sync: presets for system junk, developer folders and Git repositories (every folder with a `.git` inside, including repos created later; **Test Filters** and the review list them), and custom rules, one per line (`- *.bak` excludes, `+ ` includes). **Test Filters** says how many files a rule leaves out. Saving new filters holds the sync for a new review.
- **Options** sets how this sync behaves: when it runs, what wins in a conflict, how long replaced files are kept, and when it stops a mass delete. Each row says which default it follows; **Reset to Default** goes back to it. **Settings › Defaults** holds the defaults every sync follows. A sync that is less safe than the built-in values shows **Safety lowered**.
- **Pause** stops a sync until you resume it. **Remove Sync…** leaves the files on both sides.
- By default, files replaced on this computer by a download are kept for 30 days (**Keep replaced files** and **Keep for** in the **Options** tab, or in **Settings › Defaults**): **Open Versions Folder** in the **…** menu.

## Back up

A sync is not a backup: a deleted file is deleted on both sides. For versioned, encrypted copies of your folders, press **+** next to **Backups**; [Backing up with syncfast](backup.md) covers it, including the key you must save.

## Command line

```sh
flatpak run --command=syncfast io.github.raphael_soares.syncfast status
```

`status`, `show`, `sync`, `review` and the other commands are described in the [agent guide](../agents.md); they work for people too and print JSON with `--json`.
