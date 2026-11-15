# 0014: Sync state, Sync Now and Sync with Google Drive… in Dolphin

## Context
The Nautilus extension (ADRs 0012 and 0013) only covers Nautilus. On Fedora with KDE Plasma the file manager is Dolphin, and the wish is the same there: an emblem with the state of every file and folder inside a sync, and **Sync Now** and **Sync with Google Drive…** in the context menu. Dolphin loads only C++ plugins built against Qt 6 and KF6; it cannot run the Python extension.

## Decision
- The plugin is one more client of the session bus service of ADRs 0012 and 0013 (`Roots`, `Directory`, `SyncNow`, `Changed`). The service does not change.
- It lives in `packaging/dolphin/` (C++17, Qt 6, KF6) as two plugins over one static library: `syncfastoverlay` in `kf6/overlayicon` (a `KOverlayIconPlugin`) and `syncfastactions` in `kf6/kfileitemaction` (a `KAbstractFileItemActionPlugin`). The library holds a line by line port of the extension's decisions (`StatusCache`, `DirectoryCache`, `add_sync_target`) and the bus client.
- `getOverlays` runs on Dolphin's main thread and must not block, so the overlay plugin answers from its cache, asks `Directory` asynchronously for the folder of a file it does not know (64 folders, least recently used goes) and emits `overlaysChanged` for each path whose answer changed. On `Changed` it asks again for every cached folder; when the service goes away it clears every emblem.
- The menu plugin may be created for each menu, so it keeps no cache: it calls `Roots` synchronously with a 300 ms timeout (`Roots` answers from memory). Sync Now calls `SyncNow` without waiting; Sync with Google Drive… runs `flatpak run io.github.raphael_soares.syncfast add-sync <folder>`, as the extension does.
- Emblems are `vcs-normal` (synced), `vcs-update-required` (pending, syncing) and `vcs-conflicting` (error, conflict, attention). Breeze, the default theme on Fedora KDE, has `emblem-default` and `emblem-synchronizing` only as `-symbolic` icons, which do not read as emblems; it and Papirus have the `vcs-*` icons in colour, and the Nextcloud client uses the same names.
- There is no Sync column: Dolphin has no plugin API for one.
- It is installed from the source tree with `cmake --install` (`docs/guide/dolphin.md`); there is no RPM or COPR package. The version in `CMakeLists.txt` is bumped by hand by the owner, like the PKGBUILD.
- `just dolphin-check` builds the plugin and runs its tests. `just check` and CI do not run them, as they do not run the Nautilus extension's Python tests.
- This replaces "Fedora and other file managers are not covered" in ADR 0012 as far as Dolphin goes.

## Consequences
- The plugins loaded in Dolphin are not covered by tests. The decisions and the overlay plugin against a fake service on a session bus are.
- The decisions exist in Python and in C++: a new state on the bus, or a change to which item gets which emblem, needs both changed.
