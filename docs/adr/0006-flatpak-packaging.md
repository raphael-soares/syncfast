# 0006: Flatpak as the only distribution channel

## Context
The app must install the same way on this Arch machine and on a Fedora one, without the host providing rclone, Rust or any library. `cargo install` into `~/.local/bin` depends on the host's rclone, systemd PATH and shared libraries.

## Decision
- The only way to install is the Flatpak `io.github.raphael_soares.syncfast` (freedesktop 25.08), built from `packaging/flatpak/` by `just flatpak`; `just flatpak-bundle` exports `syncfast.flatpak`. `just install` and `cargo install` are gone.
- Channel: a GitHub Action builds on every push to `main` and publishes a GPG-signed OSTree repository to GitHub Pages (the repo is public), keeping history across runs so `flatpak update` gets deltas. Installed from the committed `.flatpakref`; `syncfast.flatpak` is a workflow artifact for offline installs. Nothing updates automatically. Quality gates stay local.
- rclone ships inside the package (official release binary, pinned sha256); crates are vendored offline from `cargo-sources.json` (`just flatpak-sources`). Nothing from the host is used but `flatpak`, and `systemctl --user` through `flatpak-spawn --host`.
- The bridge is active only when `FLATPAK_ID` is set. The units run `flatpak run --die-with-parent --command=syncfast <id> run <pair>` (and `watch`) with no `SYNCFAST_RCLONE`: the embedded rclone is on the sandbox `PATH`. `flatpak run` moves the sandbox into its own scope, so without `--die-with-parent` a `stop` or `restart` leaves the process running.
- Config, state and versions stay where they were: `Paths` ignores the remapped `XDG_*_HOME` (`HOST_XDG_*` or the `HOME` defaults) and `RCLONE_CONFIG` defaults to the host's `~/.config/rclone/rclone.conf`. The runtime folder is `$XDG_RUNTIME_DIR/app/<id>`, the only one the instances share (pair locks, instance slot).
- Permissions, each one verified: `wayland` and `fallback-x11` plus `dri` (window, GL); `network`; `home` (synced folders, rclone and syncfast config); `xdg-config/systemd:create` (`home` hides `~/.config/systemd`, so the units could not be written); `org.freedesktop.Flatpak` (`flatpak-spawn --host`); `org.freedesktop.Notifications`; `org.kde.StatusNotifierWatcher` (tray, the item registers its unique name); NetworkManager on the system bus (online check).

## Consequences
- `flatpak-spawn --host` lets the app run any host command. Accepted: it is the owner's app and systemd has no portal.
- Stopping a unit kills a running round without SIGINT; `bisync --recover --resilient` repairs the listings on the next round.
- The units need `flatpak` on the systemd user PATH and are rewritten by `apply`; a new `Cargo.lock` needs `just flatpak-sources`.
- No `--socket=x11`, so egui's X11 clipboard is unavailable in the sandbox; the app copies nothing.
- Dev mode (`just run-gui-dev`) has no `FLATPAK_ID` and is unaffected.
