# syncfast

Keeps folders on this computer in sync with Google Drive, in both directions, from one app. It runs `rclone bisync` through systemd user units that it generates, and has a tray-resident egui window to configure it.

## Install

Flatpak is the only way to install syncfast (ADR 0006). It bundles rclone and needs nothing else from the host except `flatpak` and the user's systemd. Every push to `main` publishes a signed Flatpak repository at https://raphael-soares.github.io/syncfast/.

```sh
flatpak install --user https://raphael-soares.github.io/syncfast/io.github.raphael_soares.syncfast.flatpakref
flatpak update              # later, to get the latest build (or: just flatpak-update)
flatpak run io.github.raphael_soares.syncfast
```

The runtime comes from Flathub; add it first with `flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo` if the machine has no Flathub remote.

To build from a checkout instead, `just flatpak` builds and installs for this user, and `just flatpak-bundle` also writes `syncfast.flatpak`. The Flatpak workflow uploads the same bundle as an artifact; install it offline with `flatpak install --user syncfast.flatpak`.

The command line is `flatpak run --command=syncfast io.github.raphael_soares.syncfast <apply|run|watch|preview|status>`. Start it hidden at login with `flatpak run io.github.raphael_soares.syncfast --start-hidden`.

## Development

Building the Flatpak needs `org.flatpak.Builder` (`flatpak install --user flathub org.flatpak.Builder`).

```sh
sudo pacman -S --needed just lefthook cargo-deny
cargo install --locked cargo-mutants cargo-nextest
just hooks
just check
just run-gui-dev
```

`just run-gui-dev` and `just run-cli-dev` run against an isolated `./.dev/` tree with a rclone remote (`devremote`, an alias of `./.dev/remote`) and no systemd. `just dev-reset` wipes it.

If the build reports a missing native library, install the one named in the error with `pacman -S --needed`. Likely candidates: `libxkbcommon wayland libx11 libxcursor libxrandr libxi mesa fontconfig dbus`.

The rclone log fixtures under `crates/syncfast-core/tests/fixtures/` come from the installed rclone. Regenerate them with `just capture-fixtures` after an rclone upgrade (`just capture-fixtures mixed upload` rewrites only those scenarios).

Design and decisions: `docs/spec-phase-1.md`, `docs/adr/`, and the visual reference `docs/ui-mock.html`.

## Install

```sh
just install
```

This runs `cargo install` for `syncfast` and `syncfast-gui` into `~/.local/bin`. Run `syncfast-gui --start-hidden` once (or log in again) and the app stays in the tray; it runs `syncfast apply` on start, which writes the units under `~/.config/systemd/user/` and enables `syncfast-watch.service`.

To start it with the session on Hyprland, add `hl.exec_cmd("/home/<user>/.local/bin/syncfast-gui --start-hidden")` to `autostart.lua` with the absolute path: Hyprland's `PATH` does not include `~/.local/bin`.

### Noctalia theme

Copy `contrib/noctalia/syncfast/` to `~/.local/state/noctalia/community-templates/syncfast/` and add `"syncfast"` to `community_ids` under `[theme.templates]` in Noctalia's `settings.toml`. Noctalia applies templates that are not in the community catalog: it writes `~/.config/syncfast/themes/noctalia.json` and runs `syncfast-gui reload-themes`.

## License

MIT
