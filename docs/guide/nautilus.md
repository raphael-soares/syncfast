# Nautilus emblems

Optional. On Arch Linux with Nautilus, the **syncfast-nautilus** package shows the sync state of every file and folder in your synced folders, adds a **Sync** column to the list view, and adds **Sync Now** and **Sync with Google Drive…** to the right-click menu. It does not work on other distributions or file managers. syncfast works fully without it.

## Install

syncfast must be installed as the Flatpak and running in the background (the tray icon is there). Then, from the AUR:

```sh
paru -S syncfast-nautilus
nautilus -q
```

Any AUR helper works. `nautilus -q` quits Nautilus so it loads the extension the next time you open it.

## What the emblems mean

Every file and folder inside a synced folder has one, and so does the synced folder itself. Files and folders your filters leave out have none.

- Green check: up to date.
- Arrows: changed since the last sync and waiting for the next one, or being synced now. A folder shows arrows when anything inside it changed.
- Red mark on a file: the last sync could not copy it, or it is one of the two versions kept after a conflict (or the file it was kept from). On a folder: the last sync could not copy something inside it.
- Red mark on the synced folder itself: the sync needs your attention or has conflicts. Open syncfast to see which.

## The Sync column

In the list view, open the view menu, choose **Visible Columns…** and turn on **Sync**. It says the same as the emblem in words: `Synced 5 min ago`, `Waiting`, `Syncing`, `Conflict`, or `Failed:` followed by the reason.

## Sync Now

Right-click a synced folder, or a file or folder inside one, and choose **Sync Now** to start a round of that sync without waiting for the timer. The menu does not appear outside synced folders.

If no emblem and no Sync Now appear, syncfast is not running: start it and they come back by themselves.

## Sync with Google Drive…

Right-click one folder that is not synced yet and choose **Sync with Google Drive…**. syncfast opens New Sync with that folder already filled in; pick the account and confirm. The item does not appear on a file, on more than one item, or on a folder that is inside a sync or holds one.
