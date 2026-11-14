# Nautilus emblems

Optional. On Arch Linux with Nautilus, the **syncfast-nautilus** package shows the state of your synced folders in the file manager and adds **Sync Now** to the right-click menu. It does not work on other distributions or file managers. syncfast works fully without it.

## Install

syncfast must be installed as the Flatpak and running in the background (the tray icon is there). Then, from the AUR:

```sh
paru -S syncfast-nautilus
nautilus -q
```

Any AUR helper works. `nautilus -q` quits Nautilus so it loads the extension the next time you open it.

## What the emblems mean

The emblem is on the synced folder itself, not on every file inside it.

- Green check: up to date.
- Arrows: changes are waiting to sync, or a round is running.
- Red mark: the sync needs your attention or has conflicts. Open syncfast to see which.
- A red mark on a file: it is one of the two versions kept after a conflict, or the file it was kept from.

## Sync Now

Right-click a synced folder, or a file or folder inside one, and choose **Sync Now** to start a round of that sync without waiting for the timer. The menu does not appear outside synced folders.

If no emblem and no menu appear, syncfast is not running: start it and they come back by themselves.
