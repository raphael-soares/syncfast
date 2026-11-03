# syncfast

Keeps folders on this computer in sync with Google Drive, in both directions, from one app. It runs `rclone bisync` through systemd user units that it generates, and has a tray-resident egui window to configure it.

## Development

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

## License

MIT
