# 0006: Flatpak as the only distribution channel

## Context
The app must install the same way on this Arch machine and on a Fedora one, without the host providing rclone, Rust or any library. `cargo install` into `~/.local/bin` depends on the host's rclone, systemd PATH and shared libraries.

## Decision
- The only way to install is the Flatpak `io.github.raphael_soares.syncfast` (freedesktop 25.08), built from `packaging/flatpak/` by `just flatpak` and exported as `syncfast.flatpak` by `just flatpak-bundle`. `just install` and `cargo install` are gone.
- rclone ships inside the package (official release binary, pinned sha256); crates are vendored offline from `cargo-sources.json` (`just flatpak-sources`).
- The sandbox is bridged to the host only when `FLATPAK_ID` is set. `systemctl --user` runs as `flatpak-spawn --host systemctl --user ...`. The units run `flatpak run --die-with-parent --command=syncfast <id> run <pair>` (and `watch`) with no `SYNCFAST_RCLONE`: the embedded rclone is on the sandbox `PATH`.
- Config, state and versions stay where they were: `Paths` ignores the remapped `XDG_*_HOME` (`HOST_XDG_*` or the `HOME` defaults), and `RCLONE_CONFIG` defaults to the host's `~/.config/rclone/rclone.conf`. The runtime folder is `$XDG_RUNTIME_DIR/app/<id>`, the only one the instances share (locks, instance slot).
- Permissions: Wayland and fallback X11 plus `dri` for the window; network; `home` for the synced folders and the config; `xdg-config/systemd:create` for the units; `org.freedesktop.Flatpak` for `flatpak-spawn --host`; `org.freedesktop.Notifications` and `org.kde.StatusNotifierWatcher` for notifications and the tray; NetworkManager on the system bus for the metered/offline check.
- The window and notifications use the app id as desktop entry and icon name, the window icon is `crates/syncfast-gui/assets/app-icon/`.

## Consequences
- `flatpak-spawn --host` is a sandbox escape: the app can run any host command. Accepted, it is the owner's own app and systemd has no portal.
- The units depend on `flatpak` being on the systemd user PATH (`/usr/bin`) and rerun by `apply` after an app id change.
- A new `Cargo.lock` needs `just flatpak-sources`; the Flatpak build compiles everything again.
- Dev mode (`just run-gui-dev`) is unaffected: it has no `FLATPAK_ID`.
