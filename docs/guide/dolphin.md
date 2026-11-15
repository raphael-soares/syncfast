# Dolphin emblems

Optional. With KDE Plasma's Dolphin (on Fedora or any other distribution), a plugin built from this repository shows the sync state of every file and folder in your synced folders and adds **Sync Now** and **Sync with Google Drive…** to the right-click menu. syncfast works fully without it.

## Install

syncfast must be installed as the Flatpak and running in the background (the tray icon is there). On Fedora, install the build tools once:

```sh
sudo dnf install cmake extra-cmake-modules gcc-c++ qt6-qtbase-devel kf6-kio-devel kf6-kcoreaddons-devel
```

Then, from a clone of this repository:

```sh
cmake -S packaging/dolphin -B target/dolphin-install -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr -DBUILD_TESTING=OFF
cmake --build target/dolphin-install
sudo cmake --install target/dolphin-install
```

Close every Dolphin window so it loads the plugin the next time you open it.

To remove it, from the same clone:

```sh
sudo xargs rm < target/dolphin-install/install_manifest.txt
```

## What the emblems mean

Every file and folder inside a synced folder has one, and so does the synced folder itself. Files and folders your filters leave out have none.

- Green check: up to date.
- Arrows: changed since the last sync and waiting for the next one, or being synced now. A folder shows arrows when anything inside it changed.
- Red mark on a file: the last sync could not copy it, or it is one of the two versions kept after a conflict (or the file it was kept from). On a folder: the last sync could not copy something inside it.
- Red mark on the synced folder itself: the sync needs your attention or has conflicts. Open syncfast to see which.

There is no Sync column as in Nautilus: Dolphin has no way for a plugin to add one.

## Sync Now

Right-click a synced folder, or a file or folder inside one, and choose **Sync Now** to start a round of that sync without waiting for the timer. The menu does not appear outside synced folders.

If no emblem and no Sync Now appear, syncfast is not running: start it and they come back by themselves.

## Sync with Google Drive…

Right-click one folder that is not synced yet and choose **Sync with Google Drive…**. syncfast opens New Sync with that folder already filled in; pick the account and confirm. The item does not appear on a file, on more than one item, or on a folder that is inside a sync or holds one.

If neither item ever appears in the menu, open Dolphin's **Configure Dolphin…**, go to **Context Menu** and turn on **syncfast**.
